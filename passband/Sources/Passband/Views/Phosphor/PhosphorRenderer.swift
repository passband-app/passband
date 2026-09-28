// THE P7 SQUELCH, LIVE: brand/phosphor's beam renderer (branch new-logo,
// beam.py + scope.py) ported from numpy to Metal, so the quiet sitrep can
// show it at native resolution instead of as a compressed video.
//
// Same physics, same numbers. Each frame is one sweep of the beam: the CPU
// draws its samples (the noise floor, the transient spikes, the gate's
// attenuation, the carrier) exactly as `Beam.sweep` does, the GPU splats
// them bilinearly into a phosphor buffer that decays by e^(-1/tau) a sweep,
// and the finish is `finish_p7` minus the CRT glass: a light blur, bloom at
// four radii, the P7 tonemap that saturates toward white, and the graticule
// lit by the trace's own haze.
//
// Resolution-independent by construction: the simulation runs at whatever
// working size it is given, and the two gains below put its intensities on
// the reference render's scale (1920x1200, 9000 samples a sweep), so a
// retina window and the offline stills agree.
//
// The shader ships as source and compiles at first use, like the squelch
// scene's (build.sh is bare swiftc with no Metal toolchain step).

import Metal
import MetalPerformanceShaders
import simd

/// What the scene shows. Plain values: the renderer reads a copy per frame.
struct PhosphorParams: Equatable, Sendable {
    /// Persistence in sweeps. The brand loop used 4; 12 breathes instead of
    /// strobing, which is what a backdrop under text needs.
    var tau: Float = 12
    /// The one carrier standing in the passband.
    var carrier = true
}

final class PhosphorRenderer: @unchecked Sendable {
    let device: MTLDevice
    let colorFormat: MTLPixelFormat = .bgra8Unorm
    private let queue: MTLCommandQueue
    private let decayPipeline: MTLRenderPipelineState
    private let splatPipeline: MTLRenderPipelineState
    private let compositePipeline: MTLRenderPipelineState
    private let combineKernel: MTLComputePipelineState

    // The reference render's scale, which every gain is relative to.
    private static let refW: Float = 1920
    private static let refH: Float = 1200
    private static let refSamples: Float = 9000

    // beam.py's constants, as the backdrop render set them.
    private static let center: Float = 0.5
    private static let gateW: Float = 0.2 * 0.625
    private static let carrierW: Float = 0.022 * 0.625
    private static let noise: Float = 1.25
    private static let inband: Float = 0.15
    private static let jitter: Float = 2.2
    private static let spikes = 22
    private static let exposure: Float = 5.2
    private static let glow: Float = 2.8

    // The working canvas and everything sized to it.
    private var w = 0
    private var h = 0
    private var samples = 0
    // Per-sample tables and scratch, allocated once per size: the sweep's
    // hot loop reads and writes them through raw pointers.
    private var xs = UnsafeMutablePointer<Float>.allocate(capacity: 1)
    private var gateAtten = UnsafeMutablePointer<Float>.allocate(capacity: 1)
    private var rawScratch = UnsafeMutablePointer<Float>.allocate(capacity: 1)
    private var floorScratch = UnsafeMutablePointer<Float>.allocate(capacity: 1)
    private var accum: MTLTexture?
    private var blurred: MTLTexture?
    private var scene: MTLTexture?
    private var gateTex: MTLTexture?
    private var blooms: [Level] = []
    private var haze: Level?
    private var beamBlur: MPSImageGaussianBlur?
    private var scaler: MPSImageBilinearScale?

    /// One bloom radius: the scene decimated, and blurred at that scale.
    private struct Level {
        let small: MTLTexture
        let blurred: MTLTexture
        let blur: MPSImageGaussianBlur
    }
    /// One point buffer per frame in flight. A single shared one was the
    /// first version, and the CPU overwrote each sweep while the GPU was
    /// still drawing the last: every queued frame drew the SAME sweep, and
    /// the phosphor never built up.
    private var points: [MTLBuffer] = []
    private var slot = 0
    private static let inFlight = 3
    private let frames = DispatchSemaphore(value: PhosphorRenderer.inFlight)

    // The phosphor's running normalizer: beam.py's `wsum`.
    private var wsum: Float = 0
    private var rng = SplitMix(seed: 7)
    private var fresh = true

    init?(device: MTLDevice? = MTLCreateSystemDefaultDevice()) {
        guard let device, MPSSupportsMTLDevice(device), let queue = device.makeCommandQueue(),
            let library = try? device.makeLibrary(source: phosphorShaderSource, options: nil)
        else { return nil }
        self.device = device
        self.queue = queue

        func pipeline(_ v: String, _ f: String, format: MTLPixelFormat, blend: (MTLBlendFactor, MTLBlendFactor)?)
            -> MTLRenderPipelineState?
        {
            let d = MTLRenderPipelineDescriptor()
            d.vertexFunction = library.makeFunction(name: v)
            d.fragmentFunction = library.makeFunction(name: f)
            d.colorAttachments[0].pixelFormat = format
            if let (src, dst) = blend {
                let a = d.colorAttachments[0]!
                a.isBlendingEnabled = true
                a.rgbBlendOperation = .add
                a.alphaBlendOperation = .add
                a.sourceRGBBlendFactor = src
                a.sourceAlphaBlendFactor = src
                a.destinationRGBBlendFactor = dst
                a.destinationAlphaBlendFactor = dst
            }
            return try? device.makeRenderPipelineState(descriptor: d)
        }
        guard
            // accum = accum * decay: the source contributes nothing, the
            // destination is scaled by the blend colour.
            let decay = pipeline("fullscreen_vertex", "zero_fragment", format: .r16Float, blend: (.zero, .blendColor)),
            let splat = pipeline("splat_vertex", "splat_fragment", format: .r16Float, blend: (.one, .one)),
            let composite = pipeline("fullscreen_vertex", "composite_fragment", format: colorFormat, blend: nil),
            let combineFn = library.makeFunction(name: "combine_kernel"),
            let combine = try? device.makeComputePipelineState(function: combineFn)
        else { return nil }
        decayPipeline = decay
        splatPipeline = splat
        compositePipeline = composite
        combineKernel = combine
    }

    deinit {
        for p in [xs, gateAtten, rawScratch, floorScratch, kernel] { p.deallocate() }
    }

    func makeCommandBuffer() -> MTLCommandBuffer? { queue.makeCommandBuffer() }

    // MARK: - sizing

    /// Rebuild everything sized to the working canvas. Cheap enough to run on
    /// every resize; the phosphor starts over, which reads as the trace
    /// re-settling rather than as a glitch.
    func resize(width: Int, height: Int) {
        let width = max(64, width), height = max(64, height)
        guard width != w || height != h else { return }
        w = width
        h = height
        samples = Int(Self.refSamples * Float(w) / Self.refW)
        for p in [xs, gateAtten, rawScratch, floorScratch] { p.deallocate() }
        xs = .allocate(capacity: samples)
        gateAtten = .allocate(capacity: samples)
        rawScratch = .allocate(capacity: samples)
        floorScratch = .allocate(capacity: samples)
        for i in 0..<samples {
            let x = -0.02 + 1.04 * Float(i) / Float(samples - 1)
            xs[i] = x
            gateAtten[i] = 1 - Self.response(x, Self.center, Self.gateW) * (1 - Self.inband)
        }

        func tex(_ tw: Int, _ th: Int, _ usage: MTLTextureUsage) -> MTLTexture? {
            let d = MTLTextureDescriptor.texture2DDescriptor(
                pixelFormat: .r16Float, width: tw, height: th, mipmapped: false)
            d.usage = usage
            d.storageMode = .private
            return device.makeTexture(descriptor: d)
        }
        let rw: MTLTextureUsage = [.shaderRead, .shaderWrite]
        accum = tex(w, h, [.renderTarget, .shaderRead])
        blurred = tex(w, h, rw)
        scene = tex(w, h, rw)
        gateTex = tex(w, h, [.renderTarget, .shaderRead, .shaderWrite])

        // bloom() in beam.py: each radius blurred on a copy decimated by
        // s/6, so the widest one costs a few taps instead of hundreds.
        let hs = Float(h) / 1024
        func level(_ s: Float) -> Level? {
            let px = s * hs
            let f = max(1, Int(px / 6))
            let sw = (w + f - 1) / f, sh = (h + f - 1) / f
            guard let a = tex(sw, sh, rw), let b = tex(sw, sh, rw) else { return nil }
            return Level(small: a, blurred: b, blur: MPSImageGaussianBlur(device: device, sigma: px / Float(f)))
        }
        blooms = [3, 12, 40, 110].compactMap { level($0) }
        haze = level(60)
        beamBlur = MPSImageGaussianBlur(device: device, sigma: 1.25 * hs)
        scaler = MPSImageBilinearScale(device: device)
        points = (0..<Self.inFlight).compactMap { _ in
            device.makeBuffer(length: samples * MemoryLayout<SIMD4<Float>>.stride, options: .storageModeShared)
        }
        wsum = 0
        fresh = true
    }

    // MARK: - a frame

    /// Advance one sweep and draw the frame into `target`.
    func encode(_ params: PhosphorParams, into buffer: MTLCommandBuffer, target: MTLTexture) {
        guard let accum, let blurred, let scene, let gateTex, let haze, let beamBlur,
            let scaler, blooms.count == 4, points.count == Self.inFlight
        else { return }
        // Wait for this slot's last frame to finish on the GPU before the CPU
        // writes the next sweep into it.
        frames.wait()
        let sem = frames
        buffer.addCompletedHandler { _ in sem.signal() }
        slot = (slot + 1) % Self.inFlight
        let points = self.points[slot]
        if fresh {
            drawGate(into: gateTex, buffer: buffer)
            clear(accum, buffer: buffer)
            fresh = false
        }

        // One sweep, decaying the phosphor across it: beam.py's advance(M, decay).
        let decay = exp(-1 / params.tau)
        let written = writeSweep(params, into: points, decay: decay)
        wsum = wsum * decay + written

        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = accum
        pass.colorAttachments[0].loadAction = .load
        pass.colorAttachments[0].storeAction = .store
        if let enc = buffer.makeRenderCommandEncoder(descriptor: pass) {
            enc.setRenderPipelineState(decayPipeline)
            enc.setBlendColor(red: decay, green: decay, blue: decay, alpha: decay)
            enc.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
            enc.setRenderPipelineState(splatPipeline)
            enc.setVertexBuffer(points, offset: 0, index: 0)
            var size = SIMD2<Float>(Float(w), Float(h))
            enc.setVertexBytes(&size, length: MemoryLayout<SIMD2<Float>>.size, index: 1)
            enc.drawPrimitives(type: .point, vertexStart: 0, vertexCount: samples)
            enc.endEncoding()
        }

        // intensity(): the buffer over its normalizer, lightly blurred. Then
        // finish_p7's `I * exp + gate * 0.5`, in the reference's units.
        beamBlur.encode(commandBuffer: buffer, sourceTexture: accum, destinationTexture: blurred)
        if let enc = buffer.makeComputeCommandEncoder() {
            var u = CombineUniforms(
                beamGain: Self.exposure * (Float(h) / Self.refH) / max(wsum, 1e-6),
                gateGain: 0.5 * (Float(h) / Self.refH))
            enc.setComputePipelineState(combineKernel)
            enc.setTexture(blurred, index: 0)
            enc.setTexture(gateTex, index: 1)
            enc.setTexture(scene, index: 2)
            enc.setBytes(&u, length: MemoryLayout<CombineUniforms>.size, index: 0)
            dispatch(enc, width: w, height: h)
            enc.endEncoding()
        }

        for level in blooms + [haze] {
            scaler.encode(commandBuffer: buffer, sourceTexture: scene, destinationTexture: level.small)
            level.blur.encode(commandBuffer: buffer, sourceTexture: level.small, destinationTexture: level.blurred)
        }

        let out = MTLRenderPassDescriptor()
        out.colorAttachments[0].texture = target
        out.colorAttachments[0].loadAction = .dontCare
        out.colorAttachments[0].storeAction = .store
        if let enc = buffer.makeRenderCommandEncoder(descriptor: out) {
            var u = CompositeUniforms(
                size: SIMD2(Float(w), Float(h)), glow: Self.glow, lineWidth: 1.8 * Float(h) / 1024)
            enc.setRenderPipelineState(compositePipeline)
            enc.setFragmentTexture(scene, index: 0)
            for (i, level) in blooms.enumerated() { enc.setFragmentTexture(level.blurred, index: 1 + i) }
            enc.setFragmentTexture(haze.blurred, index: 5)
            enc.setFragmentBytes(&u, length: MemoryLayout<CompositeUniforms>.size, index: 0)
            enc.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
            enc.endEncoding()
        }
    }

    // MARK: - the beam

    /// `Beam.sweep` plus the weights `advance` gives it, written straight
    /// into the point buffer. Returns the sweep's share of the normalizer.
    ///
    /// The hot loop of the whole scene, so it runs over raw pointers into
    /// scratch allocated once per size: the first version allocated two
    /// arrays a frame and iterated ranges, and an unoptimized build spent
    /// half a core on Swift's generic machinery rather than on the noise.
    private func writeSweep(_ params: PhosphorParams, into buffer: MTLBuffer, decay: Float) -> Float {
        let m = samples
        let base = 0.80 * Float(h), amp = 0.52 * Float(h)
        let W = Float(w)
        let raw = rawScratch, fl = floorScratch, xs = xs, atten = gateAtten

        // The floor: rectified gaussian jitter, smoothed by a 9-tap kernel.
        var i = 0
        while i < m {
            raw[i] = 0.028 + 0.035 * abs(rng.normal()) * Self.jitter
            i += 1
        }
        let k = kernel
        i = 0
        while i < m {
            var acc: Float = 0
            var t = 0
            while t < 9 {
                let j = i + t - 4
                if j >= 0, j < m { acc += raw[j] * k[t] }
                t += 1
            }
            fl[i] = acc
            i += 1
        }
        // The transients: digests, offers, notifications.
        let toIndex = Float(m - 1) / 1.04
        var n = 0
        while n < Self.spikes {
            let sx = rng.uniform()
            let sh = 0.08 + pow(rng.uniform(), 1.8) * 0.42
            let sw = 0.002 + rng.uniform() * 0.006
            let lo = max(0, Int((sx - 4 * sw + 0.02) * toIndex))
            let hi = min(m - 1, Int((sx + 4 * sw + 0.02) * toIndex) + 1)
            i = lo
            while i <= hi {
                let d = (xs[i] - sx) / sw
                fl[i] += sh * exp(-d * d)
                i += 1
            }
            n += 1
        }
        let shimmer = 1 + 0.004 * rng.normal()
        let per = pow(decay, 1 / Float(m))
        let p = buffer.contents().bindMemory(to: SIMD4<Float>.self, capacity: m)
        var total: Float = 0
        var weight: Float = 1  // the newest sample is the last one written
        i = m - 1
        while i >= 0 {
            let floor = fl[i] * Self.noise * atten[i]
            var y = floor
            if params.carrier {
                let d = (xs[i] - Self.center) / Self.carrierW
                let sig = 0.70 * shimmer / (1 + d * d)
                y = max(floor, sig) + min(floor, sig) * 0.3
            }
            p[i] = SIMD4(xs[i] * W, base - y * amp, weight, 0)
            total += weight
            weight *= per
            i -= 1
        }
        return total / Float(m)
    }

    /// The dashed gate outline, splatted once per size like scope.gate_line
    /// and blurred as Beam.gate does.
    private func drawGate(into texture: MTLTexture, buffer: MTLCommandBuffer) {
        let n = Int(5000 * Float(w) / Self.refW)
        let base = 0.80 * Float(h), amp = 0.52 * Float(h)
        let dash = 18 * Float(h) / 1024
        guard let pts = device.makeBuffer(length: n * MemoryLayout<SIMD4<Float>>.stride, options: .storageModeShared),
            let scratch = { () -> MTLTexture? in
                let d = MTLTextureDescriptor.texture2DDescriptor(
                    pixelFormat: .r16Float, width: w, height: h, mipmapped: false)
                d.usage = [.renderTarget, .shaderRead]
                d.storageMode = .private
                return device.makeTexture(descriptor: d)
            }()
        else { return }
        let p = pts.contents().bindMemory(to: SIMD4<Float>.self, capacity: n)
        var s: Float = 0
        var last = SIMD2<Float>(0, 0)
        for i in 0..<n {
            let x = -0.02 + 1.04 * Float(i) / Float(n - 1)
            let y = 0.02 + 0.9 * Self.response(x, Self.center, Self.gateW)
            let pt = SIMD2(x * Float(w), base - y * amp)
            if i > 0 { s += simd_distance(pt, last) }
            last = pt
            let on: Float = Int(s / dash) % 2 == 0 ? 1 : 0
            p[i] = SIMD4(pt.x, pt.y, on, 0)
        }
        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = scratch
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 0)
        pass.colorAttachments[0].storeAction = .store
        if let enc = buffer.makeRenderCommandEncoder(descriptor: pass) {
            enc.setRenderPipelineState(splatPipeline)
            enc.setVertexBuffer(pts, offset: 0, index: 0)
            var size = SIMD2<Float>(Float(w), Float(h))
            enc.setVertexBytes(&size, length: MemoryLayout<SIMD2<Float>>.size, index: 1)
            enc.drawPrimitives(type: .point, vertexStart: 0, vertexCount: n)
            enc.endEncoding()
        }
        MPSImageGaussianBlur(device: device, sigma: Float(h) / 1024)
            .encode(commandBuffer: buffer, sourceTexture: scratch, destinationTexture: texture)
    }

    private func clear(_ texture: MTLTexture, buffer: MTLCommandBuffer) {
        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = texture
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 0)
        pass.colorAttachments[0].storeAction = .store
        buffer.makeRenderCommandEncoder(descriptor: pass)?.endEncoding()
    }

    private func dispatch(_ enc: MTLComputeCommandEncoder, width: Int, height: Int) {
        let tw = combineKernel.threadExecutionWidth
        let th = max(1, combineKernel.maxTotalThreadsPerThreadgroup / tw)
        enc.dispatchThreads(
            MTLSize(width: width, height: height, depth: 1),
            threadsPerThreadgroup: MTLSize(width: tw, height: th, depth: 1))
    }

    /// scope.resp: the passband, flat top and steep 8th-order skirts.
    private static func response(_ x: Float, _ c: Float, _ w: Float) -> Float {
        1 / (1 + pow((x - c) / w, 16)).squareRoot()
    }

    /// The floor's 9-tap smoothing kernel, normalized.
    private let kernel: UnsafeMutablePointer<Float> = {
        let k = (0..<9).map { i -> Float in
            let t = -3 + 6 * Float(i) / 8
            return exp(-t * t)
        }
        let s = k.reduce(0, +)
        let p = UnsafeMutablePointer<Float>.allocate(capacity: 9)
        for i in 0..<9 { p[i] = k[i] / s }
        return p
    }()
}

private struct CombineUniforms {
    var beamGain: Float
    var gateGain: Float
}

private struct CompositeUniforms {
    var size: SIMD2<Float>
    var glow: Float
    var lineWidth: Float
}

/// A small, fast, seedable generator: the scene only needs noise that looks
/// like noise, and the system generator is neither cheap nor reproducible.
private struct SplitMix {
    private var state: UInt64
    private var spare: Float?
    init(seed: UInt64) { state = seed }

    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }

    mutating func uniform() -> Float { Float(next() >> 40) / Float(1 << 24) }

    /// Box-Muller, keeping the second value for the next call.
    mutating func normal() -> Float {
        if let s = spare { spare = nil; return s }
        let u1 = max(uniform(), 1e-7), u2 = uniform()
        let r = (-2 * log(u1)).squareRoot()
        spare = r * sin(2 * .pi * u2)
        return r * cos(2 * .pi * u2)
    }
}

private let phosphorShaderSource = """
#include <metal_stdlib>
using namespace metal;

struct FullOut { float4 position [[position]]; float2 uv; };

vertex FullOut fullscreen_vertex(uint vid [[vertex_id]]) {
    float2 p = float2((vid << 1) & 2, vid & 2);
    FullOut o;
    o.position = float4(p * 2 - 1, 0, 1);
    o.uv = float2(p.x, 1 - p.y);
    return o;
}

fragment float4 zero_fragment(FullOut in [[stage_in]]) { return float4(0); }

struct SplatOut {
    float4 position [[position]];
    float point_size [[point_size]];
    float2 at;
    float weight;
};

// One beam sample, splatted bilinearly as scope.splat does: a 3px point
// covers every pixel centre within reach, and the fragment weights each by
// its distance to the sample.
vertex SplatOut splat_vertex(uint vid [[vertex_id]],
                             const device float4 *pts [[buffer(0)]],
                             constant float2 &size [[buffer(1)]]) {
    float4 p = pts[vid];
    SplatOut o;
    // scope.splat's pixel i sits at coordinate i; Metal's centre is i + 0.5.
    float2 c = p.xy + 0.5;
    o.position = float4(c.x / size.x * 2 - 1, 1 - c.y / size.y * 2, 0, 1);
    o.point_size = 3;
    o.at = c;
    o.weight = p.z;
    return o;
}

fragment float4 splat_fragment(SplatOut in [[stage_in]]) {
    float2 d = abs(in.position.xy - in.at);
    float k = max(0.0, 1 - d.x) * max(0.0, 1 - d.y);
    return float4(in.weight * k, 0, 0, 0);
}

struct CombineUniforms { float beamGain; float gateGain; };

kernel void combine_kernel(texture2d<float, access::read> beam [[texture(0)]],
                           texture2d<float, access::read> gate [[texture(1)]],
                           texture2d<float, access::write> out [[texture(2)]],
                           constant CombineUniforms &u [[buffer(0)]],
                           uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= out.get_width() || gid.y >= out.get_height()) return;
    float v = beam.read(gid).r * u.beamGain + gate.read(gid).r * u.gateGain;
    out.write(float4(v, 0, 0, 0), gid);
}

struct CompositeUniforms { float2 size; float glow; float lineWidth; };

// scope.graticule: 16 x 10 divisions, fine ticks along both centre axes.
static float graticule(float2 p, float2 size, float lw) {
    float2 cell = size / float2(16, 10);
    float2 d = abs(fract(p / cell + 0.5) - 0.5) * cell;
    float g = max(clamp(lw - d.x, 0.0, 1.0), clamp(lw - d.y, 0.0, 1.0));
    float tick = size.x * 0.009;
    float2 fine = cell / 5;
    float2 t = abs(fract(p / fine + 0.5) - 0.5) * fine;
    if (abs(p.y - size.y / 2) < tick && t.x < lw * 0.8) g = 1;
    if (abs(p.x - size.x / 2) < tick && t.y < lw * 0.8) g = 1;
    return g;
}

fragment float4 composite_fragment(FullOut in [[stage_in]],
                                   texture2d<float> scene [[texture(0)]],
                                   texture2d<float> b0 [[texture(1)]],
                                   texture2d<float> b1 [[texture(2)]],
                                   texture2d<float> b2 [[texture(3)]],
                                   texture2d<float> b3 [[texture(4)]],
                                   texture2d<float> haze [[texture(5)]],
                                   constant CompositeUniforms &u [[buffer(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float2 uv = in.uv;
    float I = scene.sample(s, uv).r;
    float B = (0.5 * b0.sample(s, uv).r + 0.35 * b1.sample(s, uv).r
             + 0.22 * b2.sample(s, uv).r + 0.12 * b3.sample(s, uv).r) * u.glow;
    // P7: the trace saturates through blue toward white.
    float3 lit = 1 - exp(-(I * float3(0.55, 1.35, 3.0) + B * float3(0.10, 0.38, 1.10)));
    float3 bg = float3(0.020, 0.028, 0.040);
    float hz = haze.sample(s, uv).r;
    bg += graticule(uv * u.size, u.size, u.lineWidth) * float3(0.10, 0.14, 0.20) * 0.5 * (0.35 + 6 * hz);
    return float4(saturate(1 - (1 - bg) * (1 - lit)), 1);
}
"""
