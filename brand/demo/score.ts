// The film's soundtrack, synthesized: no samples, no licensing, and every cue
// is read from timeline.ts so it lands on its frame. The film's analyzer is an
// FFT of this very file, so what the phosphor trace shows is what you hear.
//
// BEFORE the squelch the inbox is a dead channel: steady, dry white hiss with
// a fixed shape (no drifting filter, no swells, no gusts, no reverb: those
// made earlier cuts sound like wind in a room), and soft pitched-noise blips, the
// notifications. They arrive sparsely in the open and in flurries whenever the
// wall scrolls, and each one is a spike on the analyzer.
//
// THE SQUELCH closes a bandpass on the hiss, and inside the band a drone
// rises: three sines stacked in fifths, 110, 165 and 247.5 Hz, which are the
// analyzer's three carriers. Nothing switches off; the noise narrows and the
// tone takes its place.
//
// AFTER is techno with no melody in it: kick (through the wall until the app
// lands), rumble and a one-note rolling bass, each in its own frequency slot,
// and live-coded tops after Yaxu and Kindohm: dry sine clicks and icy
// corkscrew blips on Euclidean patterns at phasing cycle lengths, a tight
// tonal snare, and ratchets at phrase ends. The drone runs underneath. The
// on-screen actions play the kit: typing is a run of clicks, a keystroke is
// a snare thrown into the delay, saving the rule is an ice ratchet.
//
// Clean throughout: no saturation, soft envelopes on every event, one linear
// gain to loudness (loudness.sh), never a limiter.
//
//   bun run score.ts out.wav
import { BAR, BEAT, DRONE, DURATION, FLICKS, PASS_HZ, T, typeTimes } from "./timeline";

const SR = 48000;
const N = Math.ceil(DURATION * SR);
const L = new Float32Array(N);
const R = new Float32Array(N);
const sendL = new Float32Array(N); // reverb
const sendR = new Float32Array(N);
const dubL = new Float32Array(N); // dub delay
const dubR = new Float32Array(N);

// mulberry32. An earlier LCG overflowed a double's integer range and
// collapsed into a short cycle: the noise audibly looped.
let seed = 0x51a7c0de;
const rnd = () => {
  seed |= 0; seed = (seed + 0x6d2b79f5) | 0;
  let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
};
const gauss = () => Math.sqrt(-2 * Math.log(rnd() + 1e-12)) * Math.cos(2 * Math.PI * rnd());
const clamp = (x: number, a = 0, b = 1) => Math.min(b, Math.max(a, x));
const lin = (t: number, a: number, b: number) => clamp((t - a) / (b - a));
const smooth = (x: number) => x * x * (3 - 2 * x);
const TAU = Math.PI * 2;
const onePole = (fc: number) => 1 - Math.exp((-TAU * fc) / SR);
const hann = (k: number) => (k <= 0 || k >= 1 ? 0 : 0.5 - 0.5 * Math.cos(TAU * k));
const pan = (p: number) => [Math.cos(((p + 1) * Math.PI) / 4), Math.sin(((p + 1) * Math.PI) / 4)];

/// Chamberlin state-variable bandpass, normalized to unity gain at centre.
function svf() {
  let low = 0, band = 0;
  return (x: number, fc: number, q: number) => {
    const f = 2 * Math.sin((Math.PI * Math.min(fc, SR / 6)) / SR);
    const hi = x - low - band / q;
    band += f * hi;
    low += f * band;
    return band / q;
  };
}


// ---------------------------------------------------------------- grid
const beats = (from: number, to: number) => {
  const out: number[] = [];
  const k0 = Math.ceil((from - T.appIn) / BEAT - 1e-6);
  for (let k = k0; T.appIn + k * BEAT < to; k++) out.push(T.appIn + k * BEAT);
  return out;
};
const GROOVE: Array<[number, number]> = [
  [T.appIn, T.factsIn - 0.1],
  [T.endIn - 0.05, T.fadeOut - 0.6],
];
const squelch = (t: number) => smooth(lin(t, T.squelchFrom, T.squelchTo));

// ================================================================ BEFORE
// ---------------------------------------------------------------- hiss
// White noise, dry: highpassed at 60 Hz and only softened above ~7 kHz by a
// single pole, so it is a flat, present hiss rather than the dark, roomy pink
// wash earlier cuts had. Its level only follows the scene: in with the open,
// back a step while the email wall covers the analyzer. As the squelch closes
// it passes through a bandpass centred
// on the drone, and what survives inside the band stays on as a faint floor
// under the tone, the way the site's analyzer keeps a quiet floor in-band.
{
  const bpL1 = svf(), bpL2 = svf(), bpR1 = svf(), bpR2 = svf();
  let bL = 0, bR = 0, hL = 0, hR = 0, pL = 0, pR = 0;
  const a = onePole(7000);
  for (let i = 0; i < N; i++) {
    const t = i / SR;
    // While the email wall stands in front of the analyzer the hiss steps
    // back about 6 dB, and comes up again before the squelch.
    const wall = smooth(lin(t, T.wallIn, T.wallIn + 0.8)) * (1 - smooth(lin(t, T.wallOut - 0.2, T.squelchFrom + 0.2)));
    const hp = Math.exp((-TAU * 60) / SR);
    bL += (gauss() - bL) * a;
    bR += (gauss() - bR) * a;
    hL = hp * (hL + bL - pL); pL = bL;
    hR = hp * (hR + bR - pR); pR = bR;
    const k = squelch(t);
    const q = 0.6 + 0.9 * k; // Chamberlin diverges once 1/q passes ~2
    const bandL = bpL2(bpL1(hL, PASS_HZ, q), PASS_HZ, q), bandR = bpR2(bpR1(hR, PASS_HZ, q), PASS_HZ, q);
    const outL = hL * (1 - k) + bandL * k * 1.6;
    const outR = hR * (1 - k) + bandR * k * 1.6;
    const level = 0.08 * smooth(lin(t, 0, 1.5)) * (1 - 0.5 * wall)
      * (1 - 0.8 * k) * (1 - smooth(lin(t, T.fadeOut - 0.4, DURATION)));
    L[i] += outL * level; R[i] += outR * level;
  }
}

// ---------------------------------------------------------------- blips
// Notifications: soft pitched-noise blips, 70-750 Hz, each a spike on the
// analyzer. Sparse in the open; on every flick of the wall a short flurry, as
// rows scroll past; gone by the time the buried mail glints.
function blip(at: number, g: number) {
  const dur = 0.07 + rnd() * 0.1;
  const fc = 70 * Math.pow(750 / 70, rnd());
  const [gl, gr] = pan(rnd() * 1.2 - 0.6);
  const bp = svf();
  for (let i = Math.floor(at * SR); i < Math.min(N, Math.floor((at + dur) * SR)); i++) {
    const k = (i / SR - at) / dur;
    const v = bp(gauss(), fc, 9) * hann(k) * g * 3;
    L[i] += v * gl; R[i] += v * gr;
  }
}
{
  let t0 = 1.2;
  while (t0 < T.glint) {
    const density = 1.1 + 1.6 * smooth(lin(t0, T.wallIn, T.countTo));
    blip(t0, 0.05 + rnd() * 0.04);
    t0 += 0.25 / density + (-Math.log(rnd() + 1e-9) * 0.75) / density;
  }
  for (const [st, dur, dist] of FLICKS) {
    const n = Math.round(4 + dist / 120);
    for (let j = 0; j < n; j++) {
      const at = T.wallIn + st + dur * 0.5 * Math.pow(j / n, 1.6) + rnd() * 0.02;
      blip(at, (0.035 + rnd() * 0.03) * (1 - (j / n) * 0.6));
    }
  }
}

// ---------------------------------------------------------------- glint
// When the buried mail catches the light, the middle carrier is there for a
// moment, faint, under the hiss.
{
  const a = T.glint - 0.2, b = T.wallOut + 0.8;
  for (let i = Math.floor(a * SR); i < Math.floor(b * SR); i++) {
    const t = i / SR;
    const env = smooth(lin(t, a, a + 0.7)) * (1 - smooth(lin(t, b - 0.9, b))) * 0.02;
    const v = Math.sin(TAU * PASS_HZ * t) * env;
    L[i] += v; R[i] += v;
  }
}

// ================================================================ AFTER
// ---------------------------------------------------------------- the low end
// Each low part has its own slot, so nothing stacks into mud:
//   kick    45-130 Hz, short: the body is gone within about a quarter second
//   rumble  35-100 Hz, and only BETWEEN kicks (ducked nearly to silence on each)
//   bass    E2, 82 Hz, highpassed at 60 and sidechained hard to the kick
//   drone   110 Hz up, stepping back while the groove runs
// Reverb and delay returns are highpassed so neither smears the bottom.

// ---------------------------------------------------------------- kick
// A clean pitch-swept sine with a 1 ms attack. Through the wall (lowpassed
// hard) from the squelch until the app lands.
const kickTimes: number[] = [];
const rumbleIn = new Float32Array(N);
function kick(at: number, gain: number, muffled: boolean) {
  const i0 = Math.floor(at * SR);
  let ph = 0, l1 = 0, l2 = 0;
  const a = onePole(muffled ? 160 : 12000);
  for (let i = i0; i < Math.min(N, i0 + Math.floor(SR * 0.4)); i++) {
    const t = i / SR - at;
    ph += (TAU * (45 + 85 * Math.exp(-t * 40))) / SR;
    const env = Math.min(1, t / 0.001) * Math.exp(-t * 10);
    const v = Math.sin(ph) * env * gain;
    l1 += (v - l1) * a; l2 += (l1 - l2) * a;
    const out = muffled ? l2 : v;
    L[i] += out; R[i] += out;
    if (!muffled) rumbleIn[i] += v;
  }
  kickTimes.push(at);
}
for (const at of beats(T.squelchTo + 0.4, T.appIn - 0.01)) {
  kick(at, 0.35 + 0.35 * lin(at, T.squelchTo, T.appIn), true);
}
for (const [a, b] of GROOVE) for (const at of beats(a, b)) kick(at, 0.6, false);

// Two sidechains off the same kicks: a gentle breath for the tonal parts, and
// a hard one for the bass and rumble, which must get out of the kick's way.
const duck = new Float32Array(N).fill(1);
const duckHard = new Float32Array(N).fill(1);
{
  const sorted = [...kickTimes].sort((a, b) => a - b);
  let k = -1;
  for (let i = 0; i < N; i++) {
    const t = i / SR;
    while (k + 1 < sorted.length && sorted[k + 1] <= t) k++;
    if (k < 0) continue;
    const dt = t - sorted[k];
    const attack = Math.min(1, dt / 0.004 + 0.2);
    duck[i] = 1 - 0.4 * Math.exp(-dt * 9) * attack;
    duckHard[i] = 1 - 0.92 * Math.exp(-dt * 12) * attack;
  }
}

// ---------------------------------------------------------------- drone
// Three sines in fifths, chorused a hair between the sides, rising inside the
// closing band and holding to the end. Not a melody: one sound, kept moving
// by a slow LFO on each partial at its own rate, so the chord breathes and
// shifts its weight without ever changing notes.
{
  const start = T.squelchFrom + 0.8;
  const ph = DRONE.map(() => [0, 0]);
  const lfo = [[0.07, 0.0], [0.11, 2.1], [0.053, 4.2]];
  for (let i = Math.floor(start * SR); i < N; i++) {
    const t = i / SR;
    const env = smooth(lin(t, start, T.squelchTo + 0.6)) * (1 - smooth(lin(t, T.fadeOut - 0.3, DURATION)));
    const inGroove = smooth(lin(t, T.appIn, T.appIn + BAR)) * (1 - smooth(lin(t, T.factsIn - 0.5, T.factsIn + 1)))
      + smooth(lin(t, T.endIn, T.endIn + BAR));
    const det = 0.0015 * Math.sin(TAU * 0.09 * t);
    let vl = 0, vr = 0;
    DRONE.forEach(({ f, a }, k) => {
      ph[k][0] += (TAU * f * (1 + det)) / SR;
      ph[k][1] += (TAU * f * (1 - det)) / SR;
      const m = a * (1 + 0.3 * Math.sin(TAU * lfo[k][0] * t + lfo[k][1]));
      vl += m * Math.sin(ph[k][0]);
      vr += m * Math.sin(ph[k][1]);
    });
    const g = 0.075 * env * (1 - 0.45 * inGroove) * duck[i];
    L[i] += vl * g; R[i] += vr * g;
    sendL[i] += vl * g * 0.15; sendR[i] += vr * g * 0.15;
  }
}

// ---------------------------------------------------------------- bass
// One note, E2: rhythm, not tune. What makes it a groove is everything but
// the pitch. A syncopated sixteen-step pattern leans on the offbeat eighths
// (long, accented) and answers them with short notes and ghosts; the
// off-sixteenths swing late; each note starts a touch sharp and drops into
// pitch (the bounce); and accents come in brighter, with more harmonics. The
// last bar of every four holds its final accent and drops the answer, a
// breath before the roll. Highpassed at 60 Hz so its floor is the kick's,
// its lowpass opens and closes over two bars, and it ducks hard to the kick.
const bassBus = new Float32Array(N);
const S16 = BEAT / 4;
const SWING = 0.13; // of a sixteenth, on the off-sixteenths
// [step, velocity, length in sixteenths]. The kick owns 0, 4, 8 and 12.
const BASS: Array<[number, number, number]> = [
  [1, 0.3, 0.5], [2, 1.0, 1.6], [3, 0.5, 0.6],
  [6, 1.0, 1.4], [7, 0.45, 0.5],
  [9, 0.35, 0.5], [10, 1.0, 1.7], [11, 0.55, 0.6],
  [13, 0.4, 0.5], [14, 0.95, 1.2], [15, 0.6, 0.6],
];
function bassNote(at: number, vel: number, len: number) {
  const i0 = Math.floor(at * SR);
  let ph = 0;
  for (let i = i0; i < Math.min(N, i0 + Math.floor(len * SR)); i++) {
    const t = i / SR - at;
    ph += (TAU * 82.41 * Math.pow(2, (1.5 / 12) * Math.exp(-t * 50))) / SR;
    const env = Math.min(1, t / 0.003) * Math.exp(-t * 6) * (1 - smooth(lin(t, len - 0.015, len)));
    const bright = vel * Math.exp(-t * 20);
    const v = Math.sin(ph) + (0.2 + 0.35 * bright) * Math.sin(2 * ph) + 0.15 * bright * Math.sin(3 * ph);
    bassBus[i] += v * env * 0.16 * vel;
  }
}
for (const [a, b] of GROOVE) {
  const bars = Math.floor((b - a) / BAR);
  for (let bar = 0; bar <= bars; bar++) {
    const ramp = smooth(lin(a + bar * BAR, a, a + BAR));
    const lastOfFour = bar % 4 === 3;
    for (const [step, vel, len] of BASS) {
      if (lastOfFour && step === 15) continue;
      const at = a + bar * BAR + step * S16 + (step % 2 ? SWING * S16 : 0);
      if (at >= b) continue;
      bassNote(at, vel * ramp, (lastOfFour && step === 14 ? 2 : len) * S16);
    }
  }
}
{
  let lp = 0, hp = 0, prev = 0;
  const c = Math.exp((-TAU * 60) / SR);
  for (let i = 0; i < N; i++) {
    const t = i / SR;
    const cut = 320 + 420 * (0.5 + 0.5 * Math.sin((TAU * (t - T.appIn)) / (2 * BAR) - Math.PI / 2));
    lp += (bassBus[i] - lp) * onePole(cut);
    hp = c * (hp + lp - prev); prev = lp;
    const v = hp * duckHard[i];
    L[i] += v; R[i] += v;
  }
}

// ---------------------------------------------------------------- the kit
// Tops in the live-coded manner of Tidal (Yaxu, Kindohm): dry, tonal events a
// few milliseconds long, with the interest in the PATTERN rather than the
// timbre. No noise up here at all: noise is what made the last kit fuzzy.
//
// click: a sine burst a few ms long. Pin-sharp.
// ice:   a short sine whose pitch whips down or up (the corkscrew), into
//        the dub delay.
// snare: a tonal body with a 25 ms transient: tight, not a wash.
// stut:  a ratchet, one event repeated faster and quieter, Tidal's `stut`.
//
// Patterns are Euclidean and run at different cycle lengths, so they phase
// against the bar the way Tidal patterns drift; transforms are applied by
// rule (reverse every fourth bar, rotate each bar), never by dice.

function click(at: number, gain: number, f: number, p: number, tau = 0.0016) {
  const i0 = Math.floor(at * SR);
  const [gl, gr] = pan(p);
  for (let i = i0; i < Math.min(N, i0 + Math.floor(SR * tau * 8)); i++) {
    const t = i / SR - at;
    const v = Math.sin(TAU * f * t) * Math.min(1, t / 0.0002) * Math.exp(-t / tau) * gain;
    L[i] += v * gl; R[i] += v * gr;
  }
}
function ice(at: number, gain: number, f0: number, f1: number, p: number, dub = 0.5) {
  const i0 = Math.floor(at * SR);
  const [gl, gr] = pan(p);
  let ph = 0;
  for (let i = i0; i < Math.min(N, i0 + Math.floor(SR * 0.09)); i++) {
    const t = i / SR - at;
    ph += (TAU * (f1 + (f0 - f1) * Math.exp(-t * 90))) / SR;
    const v = Math.sin(ph) * Math.min(1, t / 0.0005) * Math.exp(-t * 45) * gain;
    L[i] += v * gl; R[i] += v * gr;
    sendL[i] += v * 0.08; sendR[i] += v * 0.08;
    dubL[i] += v * dub; dubR[i] += v * dub;
  }
}
function snare(at: number, gain: number, dub = 0) {
  const i0 = Math.floor(at * SR);
  const c = Math.exp((-TAU * 3000) / SR);
  let y = 0, x1 = 0, ph = 0;
  for (let i = i0; i < Math.min(N, i0 + Math.floor(SR * 0.18)); i++) {
    const t = i / SR - at;
    ph += (TAU * 190 * (1 + 1.2 * Math.exp(-t * 60))) / SR;
    const n = gauss();
    y = c * (y + n - x1); x1 = n;
    const body = Math.sin(ph) * Math.exp(-t * 20);
    const snap = y * Math.exp(-t * 130) * 0.5;
    const v = (body + snap) * Math.min(1, t / 0.0005) * gain;
    L[i] += v; R[i] += v;
    sendL[i] += v * 0.1; sendR[i] += v * 0.1;
    dubL[i] += v * dub; dubR[i] += v * dub;
  }
}
/// Tidal's `stut`: n repeats, `every` apart, each quieter.
function stut(at: number, n: number, every: number, fire: (t: number, k: number, g: number) => void) {
  for (let k = 0; k < n; k++) fire(at + k * every, k, Math.pow(0.72, k));
}
/// Bjorklund by the modulo form: k onsets spread over n steps, rotated.
const euclid = (k: number, n: number, step: number, rot = 0) => (((step + rot) % n) * k) % n < k;

const cyc = (t: number, period: number, phase = 0) => 0.5 + 0.5 * Math.sin((TAU * t) / period + phase);
for (const [a, b] of GROOVE) {
  const steps = Math.floor((b - a) / S16);
  for (let s = 0; s < steps; s++) {
    const at = a + s * S16;
    const bar = Math.floor(s / 16), inBar = s % 16;
    const ramp = smooth(lin(at, a, a + 2 * BAR));
    // Every fourth bar the 8-step patterns play backwards: `every 4 rev`.
    const s8 = bar % 4 === 3 ? 7 - (s % 8) : s % 8;
    const jux = s % 2 ? 0.35 : -0.35;
    // The offbeat: a high click, its pitch on a four-bar LFO.
    if (inBar % 4 === 2) click(at, 0.18 * ramp, 6200 + 2200 * cyc(at - a, 4 * BAR), 0.1);
    // Euclid(5,8), rotated a step each bar: the busy, shifting click line.
    if (bar >= 1 && euclid(5, 8, s8, bar % 8)) click(at, (inBar % 4 === 0 ? 0.12 : 0.08) * ramp, 3400, jux, 0.001);
    // Euclid(3,8) on a TWELVE-step cycle: the ice line phases against the bar.
    // The corkscrew alternates down and up.
    if (bar >= 2 && euclid(3, 8, s % 12)) {
      const up = Math.floor(s / 12) % 2 === 1;
      ice(at, 0.065 * ramp, up ? 1100 : 3200, up ? 3000 : 900, -jux, 0.25 + 0.35 * cyc(at - a, 8 * BAR));
    }
    // Snare on two and four, from the third bar.
    if (bar >= 2 && (inBar === 4 || inBar === 12)) snare(at, 0.09, 0.04);
    // Phrase ends: every fourth bar a click ratchet in 64ths; every third, an
    // ice ratchet climbing out of the bar.
    if (inBar === 14 && bar % 4 === 3) stut(at, 6, S16 / 4, (t, k, g) => click(t, 0.12 * g * ramp, 5200 + 400 * k, 0));
    if (inBar === 15 && bar % 3 === 2) stut(at, 4, S16 / 2, (t, k, g) => ice(t, 0.04 * g * ramp, 1400 + 500 * k, 3600, 0.3 - 0.2 * k, 0.4));
  }
}

// The breakdown keeps the clock on offbeat clicks, with the ice line alone
// in the delay: the pattern thinned out, not stopped.
{
  const a = T.factsIn - 0.1, b = T.endIn - 0.05;
  for (let s = 0; s < Math.floor((b - a) / S16); s++) {
    const at = a + s * S16;
    const g = smooth(lin(at, T.factsIn, T.factsIn + BAR));
    if (s % 4 === 2) click(at, 0.06 * g, 7000, 0.1);
    if (euclid(3, 8, s % 12) && s % 24 < 12) ice(at, 0.03 * g, 3200, 900, s % 2 ? 0.4 : -0.4, 0.6);
  }
}

// ---------------------------------------------------------------- scene fx
// As the app lands and as the close begins: an ice ratchet, not a cymbal.
stut(T.appIn, 6, S16 / 2, (t, k, g) => ice(t, 0.05 * g, 3600 - 300 * k, 900, k % 2 ? 0.4 : -0.4, 0.5));
stut(T.endIn, 6, S16 / 2, (t, k, g) => ice(t, 0.05 * g, 3600 - 300 * k, 900, k % 2 ? 0.4 : -0.4, 0.5));

// ---------------------------------------------------------------- the actions
// e: the snare, thrown into the dub delay.
snare(T.key1, 0.1, 0.9);
snare(T.key2, 0.1, 0.9);
// t: a corkscrew up, into the delay.
ice(T.ruleKey, 0.06, 900, 3400, -0.2, 0.8);
// Typing: one pin-sharp click per character, on straight 32nds, the ones on a
// sixteenth a touch firmer.
typeTimes().forEach((at, i) => click(at, i % 2 ? 0.03 : 0.05, 5200, i % 2 ? 0.2 : -0.2, 0.001));
// Saving: an ice ratchet down, landing on the beat.
stut(T.ruleSave, 5, S16 / 2, (t, k, g) => ice(t, 0.055 * g, 3400, 800 + 150 * k, k % 2 ? 0.3 : -0.3, 0.6));

// ---------------------------------------------------------------- dub delay
// Ping-pong, dotted eighth. The repeats lose top and bottom as they go: the
// loop's highpass sits at 200 Hz, so echoes never build into low mud.
{
  const d = Math.floor(0.75 * BEAT * SR);
  const bufL = new Float32Array(d), bufR = new Float32Array(d);
  let j = 0, lpL = 0, lpR = 0, hpL = 0, hpR = 0, pL = 0, pR = 0;
  const a = onePole(2600);
  const h = Math.exp((-TAU * 200) / SR);
  for (let i = 0; i < N; i++) {
    const outL = bufL[j], outR = bufR[j];
    lpL += (outL - lpL) * a; lpR += (outR - lpR) * a;
    hpL = h * (hpL + lpL - pL); pL = lpL;
    hpR = h * (hpR + lpR - pR); pR = lpR;
    bufL[j] = dubL[i] + hpR * 0.58;
    bufR[j] = hpL * 0.58;
    j = (j + 1) % d;
    L[i] += outL * 0.7; R[i] += outR * 0.7;
    sendL[i] += outL * 0.3; sendR[i] += outR * 0.3;
  }
}

// ---------------------------------------------------------------- reverb
function reverb(input: Float32Array, spread: number, fb: number, damp: number) {
  const out = new Float32Array(N);
  const combs = [1557, 1617, 1491, 1422, 1277, 1356].map((d) => ({ buf: new Float32Array(d + spread), i: 0, lp: 0 }));
  for (let n = 0; n < N; n++) {
    let s = 0;
    for (const c of combs) {
      const y = c.buf[c.i];
      c.lp = y * (1 - damp) + c.lp * damp;
      c.buf[c.i] = input[n] + c.lp * fb;
      c.i = (c.i + 1) % c.buf.length;
      s += y;
    }
    out[n] = s / combs.length;
  }
  for (const d of [556, 441, 341]) {
    const buf = new Float32Array(d + spread);
    let j = 0;
    for (let n = 0; n < N; n++) {
      const b = buf[j];
      const y = -out[n] + b;
      buf[j] = out[n] + b * 0.5;
      j = (j + 1) % buf.length;
      out[n] = y;
    }
  }
  return out;
}
/// Two highpass poles, in place.
function highpass(x: Float32Array, fc: number) {
  const c = Math.exp((-TAU * fc) / SR);
  let y1 = 0, y2 = 0, x1 = 0, x2 = 0;
  for (let i = 0; i < N; i++) {
    const v = x[i];
    y1 = c * (y1 + v - x1); x1 = v;
    y2 = c * (y2 + y1 - x2); x2 = y1;
    x[i] = y2;
  }
}
// The space is for the mids and highs only.
highpass(sendL, 250); highpass(sendR, 250);
const wetL = reverb(sendL, 0, 0.86, 0.45), wetR = reverb(sendR, 23, 0.86, 0.45);
for (let i = 0; i < N; i++) { L[i] += wetL[i] * 0.45; R[i] += wetR[i] * 0.45; }

// The rumble: the kick through a long dark reverb, band-limited to 35-100 Hz
// and ducked almost to nothing on every kick, so it only fills the gaps.
{
  const tail = reverb(rumbleIn, 11, 0.88, 0.65);
  highpass(tail, 35);
  let a1 = 0, a2 = 0;
  const a = onePole(100);
  for (let i = 0; i < N; i++) {
    a1 += (tail[i] - a1) * a; a2 += (a1 - a2) * a;
    const v = a2 * 0.5 * duckHard[i];
    L[i] += v; R[i] += v;
  }
}

// ---------------------------------------------------------------- master
// A 28 Hz highpass (nothing useful lives below it, and it eats headroom), a
// DC blocker by the same stroke, the fades, and one gain to -1.5 dBFS. No
// shelf: the highs are meant to be crisp now.
highpass(L, 28); highpass(R, 28);
{
  let peak = 0;
  for (let i = 0; i < N; i++) {
    const t = i / SR;
    const g = smooth(lin(t, 0, 0.5)) * (1 - smooth(lin(t, T.fadeOut - 0.4, DURATION - 0.1)));
    L[i] *= g; R[i] *= g;
    peak = Math.max(peak, Math.abs(L[i]), Math.abs(R[i]));
  }
  const norm = 0.84 / peak;
  for (let i = 0; i < N; i++) { L[i] *= norm; R[i] *= norm; }
}

{
  const out = process.argv[2] ?? "score.wav";
  const buf = Buffer.alloc(44 + N * 4);
  buf.write("RIFF", 0); buf.writeUInt32LE(36 + N * 4, 4); buf.write("WAVE", 8);
  buf.write("fmt ", 12); buf.writeUInt32LE(16, 16); buf.writeUInt16LE(1, 20); buf.writeUInt16LE(2, 22);
  buf.writeUInt32LE(SR, 24); buf.writeUInt32LE(SR * 4, 28); buf.writeUInt16LE(4, 32); buf.writeUInt16LE(16, 34);
  buf.write("data", 36); buf.writeUInt32LE(N * 4, 40);
  for (let i = 0; i < N; i++) {
    buf.writeInt16LE(Math.round(clamp(L[i], -1, 1) * 32767), 44 + i * 4);
    buf.writeInt16LE(Math.round(clamp(R[i], -1, 1) * 32767), 46 + i * 4);
  }
  await Bun.write(out, buf);
  console.error(`wrote ${out}`);
}
