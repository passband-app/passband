// The film's analyzer: the landing page's phosphor scope, drawn from the
// soundtrack itself. Each frame takes an FFT of the score at that moment, so
// the hiss is the noise floor, every notification blip is a spike, and when
// the squelch closes the three peaks that stand clear are the drone's three
// partials. The site's scope.ts simulates its trace; a film has its audio to
// hand, so this one measures it. The look is scope.ts's: persistence instead
// of clearing, the trace averaged frame to frame, stroked three times
// additively, and the filter's response drawn dashed over a shaded mask.
//
// The x axis is log frequency, centred on the passband (PASS_HZ at
// layout.center), OCTAVES wide at spread 1. That puts the drone's fifths
// evenly either side of the centre, which is where the markers go.
import { DRONE, PASS_HZ, T } from "./timeline";

export type Layout = { center: number; base: number; scale: number; spread: number };

const POINTS = 520;
const BG = "9, 13, 22";
const FFT = 8192;
const OCTAVES = 6.64;
// Trace height is dB above a floor RANGE below the reference (the middle
// carrier's steady level), scaled so that reference lands at PEAK. The +3 dB
// per octave tilt is the analyzer convention for pink content: it shows the
// hiss flat instead of sloping away.
const RANGE = 46;
const PEAK = 0.7;
const TILT = 3;

export const xOf = (f: number, L: Layout) => L.center + (Math.log2(f / PASS_HZ) * L.spread) / OCTAVES;
const fOf = (x: number, L: Layout) => PASS_HZ * Math.pow(2, ((x - L.center) * OCTAVES) / L.spread);

// Flat top, steep skirts: the same response the site's scope and meter draw.
const response = (x: number, c: number, w: number) => 1 / Math.sqrt(1 + Math.pow((x - c) / w, 16));

/// The WAV the score wrote (16-bit PCM), mixed to mono.
export function decodeWav(buf: ArrayBuffer): { samples: Float32Array; sr: number } {
  const v = new DataView(buf);
  let p = 12, sr = 48000, ch = 2;
  while (p < v.byteLength - 8) {
    const id = String.fromCharCode(v.getUint8(p), v.getUint8(p + 1), v.getUint8(p + 2), v.getUint8(p + 3));
    const size = v.getUint32(p + 4, true);
    if (id === "fmt ") { ch = v.getUint16(p + 10, true); sr = v.getUint32(p + 12, true); }
    if (id === "data") {
      const n = size / (2 * ch);
      const out = new Float32Array(n);
      for (let i = 0; i < n; i++) {
        let s = 0;
        for (let c = 0; c < ch; c++) s += v.getInt16(p + 8 + (i * ch + c) * 2, true);
        out[i] = s / ch / 32768;
      }
      return { samples: out, sr };
    }
    p += 8 + size + (size & 1);
  }
  throw new Error("no data chunk in score.wav");
}

// ---------------------------------------------------------------- fft
const win = Float32Array.from({ length: FFT }, (_, i) => 0.5 - 0.5 * Math.cos((2 * Math.PI * i) / (FFT - 1)));
const rev = new Uint32Array(FFT);
for (let i = 0, bits = Math.log2(FFT); i < FFT; i++) {
  let r = 0;
  for (let b = 0; b < bits; b++) r |= ((i >> b) & 1) << (bits - 1 - b);
  rev[i] = r;
}
const cosT = Float32Array.from({ length: FFT / 2 }, (_, i) => Math.cos((2 * Math.PI * i) / FFT));
const sinT = Float32Array.from({ length: FFT / 2 }, (_, i) => -Math.sin((2 * Math.PI * i) / FFT));
const re = new Float32Array(FFT), im = new Float32Array(FFT);
const power = new Float32Array(FFT / 2);

function spectrum(samples: Float32Array, end: number) {
  for (let i = 0; i < FFT; i++) {
    const j = end - FFT + i;
    re[rev[i]] = (j >= 0 && j < samples.length ? samples[j] : 0) * win[i];
    im[rev[i]] = 0;
  }
  for (let size = 2; size <= FFT; size <<= 1) {
    const half = size >> 1, step = FFT / size;
    for (let s = 0; s < FFT; s += size) {
      for (let k = 0; k < half; k++) {
        const c = cosT[k * step], sn = sinT[k * step];
        const a = s + k, b = a + half;
        const tr = re[b] * c - im[b] * sn, ti = re[b] * sn + im[b] * c;
        re[b] = re[a] - tr; im[b] = im[a] - ti;
        re[a] += tr; im[a] += ti;
      }
    }
  }
  for (let k = 0; k < FFT / 2; k++) power[k] = re[k] * re[k] + im[k] * im[k];
}

export function createAnalyzer(canvas: HTMLCanvasElement, samples: Float32Array, sr: number) {
  const ctx = canvas.getContext("2d", { alpha: false })!;
  const width = (canvas.width = canvas.clientWidth);
  const height = (canvas.height = canvas.clientHeight);
  ctx.fillStyle = `rgb(${BG})`;
  ctx.fillRect(0, 0, width, height);
  const trace = new Float32Array(POINTS);
  const binHz = sr / FFT;

  // Power at f as dB, averaged over 1/24 octave the way a real analyzer
  // smooths: raw bins of noise are spiky, and the hiss should read as a
  // floor with the blips standing out of it, not as a floor made of spikes.
  // Where a 24th of an octave is narrower than a bin, interpolate instead.
  const dbAt = (f: number) => {
    const half = Math.pow(2, 1 / 48);
    const lo = f / half / binHz, hi = (f * half) / binHz;
    let p = 0;
    if (hi - lo < 1) {
      const b = f / binHz, i = Math.max(1, Math.floor(b)), k = b - i;
      p = power[i] * (1 - k) + power[Math.min(i + 1, power.length - 1)] * k;
    } else {
      const a = Math.max(1, Math.round(lo)), z = Math.min(power.length - 1, Math.round(hi));
      for (let i = a; i <= z; i++) p += power[i];
      p /= z - a + 1;
    }
    return 10 * Math.log10(p + 1e-12) + TILT * Math.log2(f / PASS_HZ);
  };
  // The window ends a little after the frame, so a hit shows on its frame.
  const analyse = (t: number) => spectrum(samples, Math.round(t * sr) + 1024);

  // The reference: the middle carrier's level once the drone has settled and
  // before the groove, measured rather than assumed.
  let ref = 0;
  {
    let acc = 0, n = 0;
    for (let t = T.squelchTo + 1.2; t < T.appIn - 0.2; t += 0.1) { analyse(t); acc += dbAt(PASS_HZ); n++; }
    ref = acc / n;
  }
  const level = (db: number) => Math.max(0.012, ((db - (ref - RANGE)) / RANGE) * PEAK);

  return {
    /// One film frame: fade what was there, average the new spectrum in,
    /// draw the gate and the trace.
    step(t: number, L: Layout, engage: number, dt: number) {
      analyse(t);
      const baseY = L.base * height, amp = L.scale * height;
      ctx.globalCompositeOperation = "source-over";
      ctx.fillStyle = `rgba(${BG}, ${Math.min(1, 0.36 * dt * 30)})`;
      ctx.fillRect(0, 0, width, height);

      // The filter, closing onto the drone: open is flat across the screen,
      // closed is a band just wider than the three carriers.
      const c = L.center;
      const closed = (0.95 * L.spread) / OCTAVES;
      const w = 1.6 * Math.pow(closed / 1.6, engage);
      const gate = Math.min(1, engage * 3);
      if (gate > 0.01) {
        const top = (x: number) => baseY - (0.02 + 0.84 * response(x, c, w)) * amp;
        ctx.beginPath();
        ctx.moveTo(0, baseY);
        for (let i = 0; i <= 200; i++) ctx.lineTo((i / 200) * width, top(i / 200));
        ctx.lineTo(width, baseY);
        ctx.closePath();
        const fill = ctx.createLinearGradient(0, baseY - amp, 0, baseY);
        fill.addColorStop(0, `rgba(78, 155, 234, ${0.07 * gate})`);
        fill.addColorStop(1, "rgba(78, 155, 234, 0)");
        ctx.fillStyle = fill;
        ctx.fill();
        ctx.beginPath();
        for (let i = 0; i <= 200; i++) {
          const x = (i / 200) * width;
          if (i === 0) ctx.moveTo(x, top(0));
          else ctx.lineTo(x, top(i / 200));
        }
        ctx.setLineDash([4, 5]);
        ctx.strokeStyle = `rgba(130, 186, 245, ${0.5 * gate})`;
        ctx.lineWidth = 1;
        ctx.stroke();
        ctx.setLineDash([]);
      }

      const k = 1 - Math.exp(-dt * 16);
      for (let i = 0; i < POINTS; i++) {
        const f = fOf(i / (POINTS - 1), L);
        // Below ~28 Hz the FFT has too few bins to draw anything but straight
        // lines, and nothing audible lives there: fade it to the floor.
        const lowCut = Math.min(1, Math.max(0, (f - 18) / 12));
        const v = f < 18 || f > sr / 2 ? 0.012 : 0.012 + (level(dbAt(f)) - 0.012) * lowCut * lowCut;
        trace[i] += (v - trace[i]) * k;
      }
      ctx.globalCompositeOperation = "lighter";
      const path = new Path2D();
      for (let i = 0; i < POINTS; i++) {
        const x = (i / (POINTS - 1)) * width;
        const y = baseY - Math.min(1, trace[i]) * amp;
        if (i === 0) path.moveTo(x, y);
        else path.lineTo(x, y);
      }
      ctx.lineJoin = "round";
      for (const [lw, color] of [[9, "rgba(78, 155, 234, 0.05)"], [3.5, "rgba(78, 155, 234, 0.28)"], [1.2, "rgba(214, 233, 255, 0.85)"]] as const) {
        ctx.lineWidth = lw;
        ctx.strokeStyle = color;
        ctx.stroke(path);
      }
      ctx.globalCompositeOperation = "source-over";
    },
    /// Where carrier i's peak is on the trace right now, as canvas fractions.
    peak(i: number, L: Layout) {
      const x = xOf(DRONE[i].f, L);
      const c = Math.round(x * (POINTS - 1));
      let best = 0;
      for (let j = Math.max(0, c - 3); j <= Math.min(POINTS - 1, c + 3); j++) best = Math.max(best, trace[j]);
      return { x, y: L.base - Math.min(1, best) * L.scale };
    },
  };
}
