"""A widescreen, glassless cut of brand/phosphor's P7 squelch for the quiet
sitrep's backdrop: the gate held open (actively squelching), noise outside it,
the carrier standing. Same engine and P7 tonemap; only the canvas changes.

Needs brand/phosphor (branch new-logo) and its requirements:

    PHOSPHOR=../brand/phosphor python3 Tools/render-p7-backdrop.py still out.png
    PHOSPHOR=../brand/phosphor SECS=8 python3 Tools/render-p7-backdrop.py loop out
    ffmpeg -framerate 30 -i out.frames/%04d.png -c:v libx265 -tag:v hvc1 \
        -pix_fmt yuv420p -crf 26 -preset slow Sources/Passband/Resources/Brand/P7Squelch.mp4

~2.3 s a frame at 1920x1200. CARRIER=0 renders the band empty."""
import sys, os, time, shutil, subprocess
sys.path.insert(0, os.environ['PHOSPHOR'])
import numpy as np
from scipy.ndimage import gaussian_filter
from PIL import Image
from beam import Beam, bloom, P7
from scope import resp, gate_line, graticule

class Wide(Beam):
    def __init__(self, W, H, *, base, amp, **kw):
        super().__init__(H, base=0, amp=0, **kw)
        self.W, self.H = W, H
        self.base, self.amp = base, amp
        self.buf = np.zeros((H, W))
    def intensity(self):
        return gaussian_filter(self.buf / max(self.wsum, 1e-9), 1.25 * self.H / 1024)
    def gate(self, gh=0.9, dash=18):
        G = gate_line(self.W, self.H, base=self.base, amp=self.amp, gate_w=self.gate_w, gh=gh,
                      center=self.center, dash=dash * self.H / 1024)
        return gaussian_filter(G, self.H / 1024)

def finish(b, *, exp=5.2, glow=2.8, gate=0.5, grat=0.5):
    I = b.intensity() * exp + b.gate() * gate
    B = bloom(I) * glow
    lit = 1 - np.exp(-(I[..., None] * np.array(P7['core']) + B[..., None] * np.array(P7['glow'])))
    bg = np.ones((b.H, b.W, 3)) * np.array(P7['bg'])
    if grat:
        haze = gaussian_filter(I, 60 * b.H / 1024)
        bg += graticule(b.W, b.H, div=(16, 10), lw=1.8 * b.H / 1024)[..., None] * np.array(P7['grat']) * grat * (0.35 + 6 * haze[..., None])
    return np.clip(1 - (1 - bg) * (1 - lit), 0, 1)

W, H = int(os.environ.get('W', 1920)), int(os.environ.get('H', 1200))
carrier = os.environ.get('CARRIER', '1') == '1'
def beam():
    return Wide(W, H, base=0.80 * H, amp=0.52 * H,
                carriers=[dict(dx=0, h=0.70)] if carrier else [], cw=0.022 * 0.625,
                gate_w=0.2 * 0.625, spikes=22, M=9000, center=0.5)

mode = sys.argv[1]
if mode == 'still':
    b = beam()
    for _ in range(20): b.advance(b.M, np.exp(-1 / 4))
    t = time.time(); f = finish(b); print(f'{time.time()-t:.2f}s/frame')
    Image.fromarray((f * 255 + .5).astype(np.uint8)).save(sys.argv[2])
else:
    FPS, secs, xf = 30, float(os.environ.get('SECS', 8)), 15
    n = int(FPS * secs)
    b = beam()
    for _ in range(20): b.advance(b.M, np.exp(-1 / 4))
    frames = []
    for i in range(n + xf):
        b.advance(b.M, np.exp(-1 / 4))
        frames.append(finish(b)); print(f'\r{i}', end='', flush=True)
    # Seamless loop: the tail crossfades into the head.
    for i in range(xf):
        a = (i + 1) / (xf + 1)
        frames[i] = frames[i] * a + frames[n + i] * (1 - a)
    d = sys.argv[2] + '.frames'; shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
    for i in range(n):
        Image.fromarray((frames[i] * 255 + .5).astype(np.uint8)).save(f'{d}/{i:04d}.png')
    print()
