import AppKit
import AudioToolbox
import CoreAudio
import Foundation

private let lowyapTapRingCapacity = 30 * 16_000

private let pureMeetingBundles: [String] = [
    "us.zoom.xos",
    "com.microsoft.teams2",
    "com.microsoft.teams",
    "com.apple.FaceTime",
    "com.cisco.webexmeetingsapp",
    "app.tuple.app",
    "co.around.Around",
]

private let inputRequiredCallBundles: [String] = [
    "com.tinyspeck.slackmacgap",
    "com.hnc.Discord",
    "com.loom.desktop",
]

private let dedicatedCallBundles: [String] = pureMeetingBundles + inputRequiredCallBundles

private let browserCallBundles: [String] = [
    "com.google.Chrome",
    "company.thebrowser.Browser",
    "com.apple.Safari",
    "com.brave.Browser",
    "org.mozilla.firefox",
    "com.microsoft.edgemac",
]

private let headphoneNameTokens: [String] = [
    "headphone",
    "airpods",
    "earpods",
    "headset",
    "buds",
    "earbuds",
    "beats",
    "wh-1000",
    "wf-1000",
    "quietcomfort",
    "bose qc",
    "jabra",
    "sennheiser",
    "nothing ear",
]

private func defaultDeviceID(selector: AudioObjectPropertySelector) -> AudioObjectID? {
    var address = AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    var device = AudioObjectID(kAudioObjectUnknown)
    let status = AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject),
        &address,
        0,
        nil,
        &size,
        &device
    )
    guard status == noErr, device != kAudioObjectUnknown else {
        return nil
    }
    return device
}

private func readUInt32Property(
    objectID: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal
) -> UInt32? {
    var address = AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain
    )
    var value: UInt32 = 0
    var size = UInt32(MemoryLayout<UInt32>.size)
    let status = AudioObjectGetPropertyData(objectID, &address, 0, nil, &size, &value)
    return status == noErr ? value : nil
}

private func readStringProperty(
    objectID: AudioObjectID,
    selector: AudioObjectPropertySelector
) -> String? {
    var address = AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var unmanagedValue: Unmanaged<CFString>?
    var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    let status = AudioObjectGetPropertyData(objectID, &address, 0, nil, &size, &unmanagedValue)
    guard status == noErr, let cfStr = unmanagedValue?.takeRetainedValue() else {
        return nil
    }
    return cfStr as String
}

private func fourCC(_ value: UInt32) -> String {
    let b0 = UInt8((value >> 24) & 0xFF)
    let b1 = UInt8((value >> 16) & 0xFF)
    let b2 = UInt8((value >> 8) & 0xFF)
    let b3 = UInt8(value & 0xFF)
    return String(bytes: [b0, b1, b2, b3], encoding: .ascii) ?? ""
}

private func writeCString(_ text: String, into out: UnsafeMutablePointer<CChar>?, cap: Int32) {
    guard let out, cap > 0 else { return }
    let capacity = Int(cap)
    var bytes = Array(text.utf8)
    if bytes.count >= capacity {
        bytes = Array(bytes.prefix(max(0, capacity - 1)))
    }
    for (idx, byte) in bytes.enumerated() {
        out[idx] = CChar(bitPattern: byte)
    }
    out[bytes.count] = 0
}

private func detectHeadphonesByName(_ name: String?) -> Bool {
    guard let lowered = name?.lowercased() else { return false }
    return headphoneNameTokens.contains { lowered.contains($0) }
}

private final class FloatRingBuffer {
    private let capacity: Int
    private var storage: [Float]
    private var readIndex = 0
    private var writeIndex = 0
    private var count = 0
    private let lock = NSLock()

    init(capacity: Int) {
        self.capacity = max(capacity, 1)
        self.storage = Array(repeating: 0, count: self.capacity)
    }

    func push(_ samples: [Float]) {
        guard !samples.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }

        for sample in samples {
            if count == capacity {
                readIndex = (readIndex + 1) % capacity
                count -= 1
            }
            storage[writeIndex] = sample
            writeIndex = (writeIndex + 1) % capacity
            count += 1
        }
    }

    func drain(into out: UnsafeMutablePointer<Float>?, maxSamples: Int) -> Int32 {
        guard maxSamples > 0 else { return 0 }
        lock.lock()
        defer { lock.unlock() }

        let amount = min(maxSamples, count)
        guard amount > 0 else { return 0 }

        if let out {
            for idx in 0..<amount {
                out[idx] = storage[readIndex]
                readIndex = (readIndex + 1) % capacity
            }
        } else {
            readIndex = (readIndex + amount) % capacity
        }
        count -= amount
        return Int32(amount)
    }
}

private final class StreamingDownsampler {
    private var sourceRate: Double
    private var outputPerInput: Double
    private var accumulator: Double = 0
    private var previous: Float = 0
    private var prevRaw1: Float = 0
    private var prevRaw2: Float = 0
    private var hasPrevious = false

    init(sourceRate: Double) {
        self.sourceRate = max(sourceRate, 1)
        self.outputPerInput = 16_000.0 / self.sourceRate
    }

    func updateSourceRate(_ rate: Double) {
        let next = max(rate, 1)
        if abs(next - sourceRate) < 0.5 { return }
        sourceRate = next
        outputPerInput = 16_000.0 / sourceRate
        accumulator = 0
        hasPrevious = false
    }

    func process(_ input: [Float]) -> [Float] {
        guard !input.isEmpty else { return [] }
        if abs(sourceRate - 16_000.0) < 1.0 {
            return input
        }

        let useLowPass = sourceRate >= 32_000.0
        var output: [Float] = []
        output.reserveCapacity(max(1, Int(Double(input.count) * outputPerInput) + 2))

        for rawSample in input {
            if !hasPrevious {
                previous = rawSample
                prevRaw1 = rawSample
                prevRaw2 = rawSample
                hasPrevious = true
            }
            let sample: Float
            if useLowPass {
                sample = 0.25 * prevRaw2 + 0.5 * prevRaw1 + 0.25 * rawSample
                prevRaw2 = prevRaw1
                prevRaw1 = rawSample
            } else {
                sample = rawSample
            }

            accumulator += outputPerInput
            if accumulator >= 1.0 {
                let produced = Int(accumulator.rounded(.down))
                let denom = Float(max(produced, 1))
                for idx in 0..<produced {
                    let t = Float(idx + 1) / denom
                    output.append(previous + (sample - previous) * t)
                }
                accumulator -= Double(produced)
            }
            previous = sample
        }

        return output
    }
}

private func decodeMonoFloats(
    _ inputData: UnsafePointer<AudioBufferList>,
    format: AudioStreamBasicDescription
) -> [Float] {
    let buffers = UnsafeMutableAudioBufferListPointer(
        UnsafeMutablePointer<AudioBufferList>(mutating: inputData)
    )
    guard !buffers.isEmpty else { return [] }

    let channels = Int(max(format.mChannelsPerFrame, 1))
    let formatIsPCM = format.mFormatID == kAudioFormatLinearPCM
    let isFloat = formatIsPCM && (format.mFormatFlags & kAudioFormatFlagIsFloat) != 0
    let isSignedInt = formatIsPCM && (format.mFormatFlags & kAudioFormatFlagIsSignedInteger) != 0
    let isNonInterleaved = formatIsPCM && (format.mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0

    let bytesPerSample = Int(max(format.mBitsPerChannel / 8, 1))
    let bytesPerFrame = Int(max(format.mBytesPerFrame, UInt32(bytesPerSample * channels)))

    if isFloat, format.mBitsPerChannel == 32 {
        if isNonInterleaved {
            let perChannel = min(
                buffers.count,
                channels
            )
            guard perChannel > 0 else { return [] }
            let frameCount = Int(buffers[0].mDataByteSize) / bytesPerSample
            var mono = Array(repeating: Float(0), count: frameCount)
            for channelIdx in 0..<perChannel {
                guard let mData = buffers[channelIdx].mData else { continue }
                let ptr = mData.assumingMemoryBound(to: Float.self)
                for frame in 0..<frameCount {
                    mono[frame] += ptr[frame]
                }
            }
            let denom = Float(perChannel)
            return mono.map { $0 / denom }
        }

        guard let mData = buffers[0].mData else { return [] }
        let ptr = mData.assumingMemoryBound(to: Float.self)
        let frameCount = Int(buffers[0].mDataByteSize) / bytesPerFrame
        var mono = Array(repeating: Float(0), count: frameCount)
        for frame in 0..<frameCount {
            var acc: Float = 0
            let base = frame * channels
            for channel in 0..<channels {
                acc += ptr[base + channel]
            }
            mono[frame] = acc / Float(channels)
        }
        return mono
    }

    if isSignedInt, format.mBitsPerChannel == 16 {
        let scale = Float(Int16.max)
        if isNonInterleaved {
            let perChannel = min(buffers.count, channels)
            guard perChannel > 0 else { return [] }
            let frameCount = Int(buffers[0].mDataByteSize) / bytesPerSample
            var mono = Array(repeating: Float(0), count: frameCount)
            for channelIdx in 0..<perChannel {
                guard let mData = buffers[channelIdx].mData else { continue }
                let ptr = mData.assumingMemoryBound(to: Int16.self)
                for frame in 0..<frameCount {
                    mono[frame] += Float(ptr[frame]) / scale
                }
            }
            let denom = Float(perChannel)
            return mono.map { $0 / denom }
        }

        guard let mData = buffers[0].mData else { return [] }
        let ptr = mData.assumingMemoryBound(to: Int16.self)
        let frameCount = Int(buffers[0].mDataByteSize) / bytesPerFrame
        var mono = Array(repeating: Float(0), count: frameCount)
        for frame in 0..<frameCount {
            var acc: Float = 0
            let base = frame * channels
            for channel in 0..<channels {
                acc += Float(ptr[base + channel]) / scale
            }
            mono[frame] = acc / Float(channels)
        }
        return mono
    }

    if isSignedInt, format.mBitsPerChannel == 32 {
        let scale = Float(Int32.max)
        if isNonInterleaved {
            let perChannel = min(buffers.count, channels)
            guard perChannel > 0 else { return [] }
            let frameCount = Int(buffers[0].mDataByteSize) / bytesPerSample
            var mono = Array(repeating: Float(0), count: frameCount)
            for channelIdx in 0..<perChannel {
                guard let mData = buffers[channelIdx].mData else { continue }
                let ptr = mData.assumingMemoryBound(to: Int32.self)
                for frame in 0..<frameCount {
                    mono[frame] += Float(ptr[frame]) / scale
                }
            }
            let denom = Float(perChannel)
            return mono.map { $0 / denom }
        }

        guard let mData = buffers[0].mData else { return [] }
        let ptr = mData.assumingMemoryBound(to: Int32.self)
        let frameCount = Int(buffers[0].mDataByteSize) / bytesPerFrame
        var mono = Array(repeating: Float(0), count: frameCount)
        for frame in 0..<frameCount {
            var acc: Float = 0
            let base = frame * channels
            for channel in 0..<channels {
                acc += Float(ptr[base + channel]) / scale
            }
            mono[frame] = acc / Float(channels)
        }
        return mono
    }

    return []
}

@available(macOS 14.2, *)
private final class SystemTapContext {
    private let ring = FloatRingBuffer(capacity: lowyapTapRingCapacity)
    private let downsampler: StreamingDownsampler
    private var isStopped = false

    var tapID: AudioObjectID
    var aggregateID: AudioObjectID
    var ioProcID: AudioDeviceIOProcID?
    var format: AudioStreamBasicDescription

    init(
        tapID: AudioObjectID,
        aggregateID: AudioObjectID,
        format: AudioStreamBasicDescription
    ) {
        self.tapID = tapID
        self.aggregateID = aggregateID
        self.format = format
        self.downsampler = StreamingDownsampler(sourceRate: Double(format.mSampleRate))
    }

    func ingest(inInputData: UnsafePointer<AudioBufferList>?) {
        guard let inInputData else { return }
        downsampler.updateSourceRate(Double(format.mSampleRate))
        let mono = decodeMonoFloats(inInputData, format: format)
        guard !mono.isEmpty else { return }
        ring.push(downsampler.process(mono))
    }

    func drain(into out: UnsafeMutablePointer<Float>?, maxSamples: Int) -> Int32 {
        ring.drain(into: out, maxSamples: maxSamples)
    }

    func stop() {
        guard !isStopped else { return }
        isStopped = true

        if let ioProcID {
            _ = AudioDeviceStop(aggregateID, ioProcID)
            _ = AudioDeviceDestroyIOProcID(aggregateID, ioProcID)
            self.ioProcID = nil
        }
        if aggregateID != kAudioObjectUnknown {
            _ = AudioHardwareDestroyAggregateDevice(aggregateID)
            aggregateID = kAudioObjectUnknown
        }
        if tapID != kAudioObjectUnknown {
            _ = AudioHardwareDestroyProcessTap(tapID)
            tapID = kAudioObjectUnknown
        }
    }
}

@available(macOS 14.2, *)
private struct ActiveCallMatch {
    let kind: Int32
    let bundleID: String
    let appName: String
}

@available(macOS 14.2, *)
private func readProcessPID(objectID: AudioObjectID) -> pid_t? {
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioProcessPropertyPID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var pid: pid_t = 0
    var size = UInt32(MemoryLayout<pid_t>.size)
    let status = AudioObjectGetPropertyData(objectID, &address, 0, nil, &size, &pid)
    guard status == noErr, pid > 0 else {
        return nil
    }
    return pid
}

@available(macOS 14.2, *)
private func bundleMatches(_ candidate: String, canonical: String) -> Bool {
    if candidate == canonical || candidate.hasPrefix(canonical + ".") {
        return true
    }
    if canonical == "us.zoom.xos" && candidate.hasPrefix("us.zoom.") {
        return true
    }
    if canonical == "com.cisco.webexmeetingsapp" && candidate.hasPrefix("com.cisco.webex") {
        return true
    }
    if canonical == "com.apple.Safari" && candidate.hasPrefix("com.apple.WebKit") {
        return true
    }
    return false
}

@available(macOS 14.2, *)
private func detectActiveAudioCallApp() -> ActiveCallMatch? {
    var listAddress = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyProcessObjectList,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var byteSize: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(
        AudioObjectID(kAudioObjectSystemObject),
        &listAddress,
        0,
        nil,
        &byteSize
    ) == noErr, byteSize >= UInt32(MemoryLayout<AudioObjectID>.size) else {
        return nil
    }

    let count = Int(byteSize) / MemoryLayout<AudioObjectID>.size
    var processObjects = Array(repeating: AudioObjectID(kAudioObjectUnknown), count: count)
    guard AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject),
        &listAddress,
        0,
        nil,
        &byteSize,
        &processObjects
    ) == noErr else {
        return nil
    }

    let selfPID = getpid()
    let runningApps = NSWorkspace.shared.runningApplications.filter { !$0.isTerminated }

    var activeBundles: [(bundle: String, isInput: Bool, isOutput: Bool)] = []
    for obj in processObjects where obj != kAudioObjectUnknown {
        if let pid = readProcessPID(objectID: obj), pid == selfPID {
            continue
        }
        let isInput = (readUInt32Property(
            objectID: obj,
            selector: kAudioProcessPropertyIsRunningInput
        ) ?? 0) != 0
        let isOutput = (readUInt32Property(
            objectID: obj,
            selector: kAudioProcessPropertyIsRunningOutput
        ) ?? 0) != 0
        guard isInput || isOutput else { continue }

        var resolvedBundle = readStringProperty(
            objectID: obj,
            selector: kAudioProcessPropertyBundleID
        ) ?? ""
        if resolvedBundle.isEmpty, let pid = readProcessPID(objectID: obj) {
            resolvedBundle = NSRunningApplication(processIdentifier: pid)?.bundleIdentifier ?? ""
        }
        guard !resolvedBundle.isEmpty else { continue }
        activeBundles.append((bundle: resolvedBundle, isInput: isInput, isOutput: isOutput))
    }

    guard !activeBundles.isEmpty else { return nil }

    // 1. Dedicated pure meeting apps (Zoom, Teams, FaceTime, Webex, Tuple, Around):
    // active input OR active call output (catches joining a call with mic muted).
    for canonical in pureMeetingBundles {
        guard let app = runningApps.first(where: { $0.bundleIdentifier == canonical }) else {
            continue
        }
        if activeBundles.contains(where: {
            bundleMatches($0.bundle, canonical: canonical) && ($0.isInput || $0.isOutput)
        }) {
            let bundleID = app.bundleIdentifier ?? canonical
            let appName = app.localizedName ?? bundleID
            return ActiveCallMatch(kind: 1, bundleID: bundleID, appName: appName)
        }
    }

    // 2. Hybrid chat/recording apps (Slack, Discord, Loom): require active mic input
    // so notification chimes never trigger call detection.
    for canonical in inputRequiredCallBundles {
        guard let app = runningApps.first(where: { $0.bundleIdentifier == canonical }) else {
            continue
        }
        if activeBundles.contains(where: {
            bundleMatches($0.bundle, canonical: canonical) && $0.isInput
        }) {
            let bundleID = app.bundleIdentifier ?? canonical
            let appName = app.localizedName ?? bundleID
            return ActiveCallMatch(kind: 1, bundleID: bundleID, appName: appName)
        }
    }

    // 3. Browsers (Meet / Teams Web / Zoom Web): strictly require active mic input
    // so YouTube / music playback never triggers call detection.
    for canonical in browserCallBundles {
        guard let app = runningApps.first(where: { $0.bundleIdentifier == canonical }) else {
            continue
        }
        if activeBundles.contains(where: {
            bundleMatches($0.bundle, canonical: canonical) && $0.isInput
        }) {
            let bundleID = app.bundleIdentifier ?? canonical
            let appName = app.localizedName ?? bundleID
            return ActiveCallMatch(kind: 2, bundleID: bundleID, appName: appName)
        }
    }

    return nil
}

@_cdecl("lowyap_is_default_input_running")
public func lowyap_is_default_input_running() -> Int32 {
    if #available(macOS 14.2, *) {
        return detectActiveAudioCallApp() != nil ? 1 : 0
    }
    guard let inputID = defaultDeviceID(selector: kAudioHardwarePropertyDefaultInputDevice) else {
        return 0
    }
    let running = readUInt32Property(
        objectID: inputID,
        selector: kAudioDevicePropertyDeviceIsRunningSomewhere
    ) ?? 0
    return running == 0 ? 0 : 1
}

@_cdecl("lowyap_detect_output_route")
public func lowyap_detect_output_route() -> Int32 {
    guard let outputID = defaultDeviceID(selector: kAudioHardwarePropertyDefaultOutputDevice) else {
        return 0
    }

    let transport = readUInt32Property(objectID: outputID, selector: kAudioDevicePropertyTransportType)
    let dataSource = readUInt32Property(
        objectID: outputID,
        selector: kAudioDevicePropertyDataSource,
        scope: kAudioDevicePropertyScopeOutput
    )
    let deviceName = readStringProperty(objectID: outputID, selector: kAudioObjectPropertyName)
    let isHeadphonesByName = detectHeadphonesByName(deviceName)
    let isHeadphonesDataSource = dataSource.map { fourCC($0).lowercased() == "hdpn" } ?? false

    if isHeadphonesByName || isHeadphonesDataSource {
        return 1
    }

    if let transport {
        if transport == kAudioDeviceTransportTypeBluetooth
            || transport == kAudioDeviceTransportTypeBluetoothLE
        {
            // Unnamed Bluetooth devices may be speakers/soundbars; return 0 (Unknown)
            // so SpeakerEchoGuard remains active unless identified as headphones.
            return 0
        }

        if transport == kAudioDeviceTransportTypeUSB {
            return 0
        }

        if transport == kAudioDeviceTransportTypeBuiltIn
            || transport == kAudioDeviceTransportTypeHDMI
            || transport == kAudioDeviceTransportTypeDisplayPort
        {
            return 2
        }
    }

    return 0
}

@_cdecl("lowyap_detect_running_call_app")
public func lowyap_detect_running_call_app(
    _ out_bundle: UnsafeMutablePointer<CChar>?,
    bundle_cap: Int32,
    _ out_name: UnsafeMutablePointer<CChar>?,
    name_cap: Int32
) -> Int32 {
    if #available(macOS 14.2, *) {
        if let active = detectActiveAudioCallApp() {
            writeCString(active.bundleID, into: out_bundle, cap: bundle_cap)
            writeCString(active.appName, into: out_name, cap: name_cap)
            return active.kind
        }
        writeCString("", into: out_bundle, cap: bundle_cap)
        writeCString("", into: out_name, cap: name_cap)
        return 0
    }

    let runningApps = NSWorkspace.shared.runningApplications.filter { !$0.isTerminated }

    for bundle in dedicatedCallBundles {
        guard let app = runningApps.first(where: { $0.bundleIdentifier == bundle }) else {
            continue
        }
        let bundleID = app.bundleIdentifier ?? bundle
        let appName = app.localizedName ?? bundleID
        writeCString(bundleID, into: out_bundle, cap: bundle_cap)
        writeCString(appName, into: out_name, cap: name_cap)
        return 1
    }

    for bundle in browserCallBundles {
        guard let app = runningApps.first(where: { $0.bundleIdentifier == bundle }) else {
            continue
        }
        let bundleID = app.bundleIdentifier ?? bundle
        let appName = app.localizedName ?? bundleID
        writeCString(bundleID, into: out_bundle, cap: bundle_cap)
        writeCString(appName, into: out_name, cap: name_cap)
        return 2
    }

    writeCString("", into: out_bundle, cap: bundle_cap)
    writeCString("", into: out_name, cap: name_cap)
    return 0
}

@available(macOS 14.2, *)
private func lowyap_translate_pid_to_process_object() -> AudioObjectID? {
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var pid = getpid()
    var processObject = AudioObjectID(kAudioObjectUnknown)
    var size = UInt32(MemoryLayout<AudioObjectID>.size)

    let status = AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject),
        &address,
        UInt32(MemoryLayout<pid_t>.size),
        &pid,
        &size,
        &processObject
    )
    guard status == noErr, processObject != kAudioObjectUnknown else {
        return nil
    }
    return processObject
}

@_cdecl("lowyap_system_tap_start")
public func lowyap_system_tap_start() -> UnsafeMutableRawPointer? {
    guard #available(macOS 14.2, *) else {
        return nil
    }

    let excludedProcesses = lowyap_translate_pid_to_process_object().map { [$0] } ?? []
    let tapDescription = CATapDescription(monoGlobalTapButExcludeProcesses: excludedProcesses)
    tapDescription.uuid = UUID()
    tapDescription.isPrivate = true
    tapDescription.muteBehavior = .unmuted

    var tapID = AudioObjectID(kAudioObjectUnknown)
    guard AudioHardwareCreateProcessTap(tapDescription, &tapID) == noErr else {
        return nil
    }

    var tapUID = tapDescription.uuid.uuidString
    do {
        var uidAddress = AudioObjectPropertyAddress(
            mSelector: kAudioTapPropertyUID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var uidCF: CFString = tapUID as CFString
        var uidSize = UInt32(MemoryLayout<CFString>.size)
        if AudioObjectGetPropertyData(tapID, &uidAddress, 0, nil, &uidSize, &uidCF) == noErr {
            tapUID = uidCF as String
        }
    }

    var tapFormat = AudioStreamBasicDescription(
        mSampleRate: 48_000,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat
            | kAudioFormatFlagIsPacked
            | kAudioFormatFlagIsNonInterleaved,
        mBytesPerPacket: 4,
        mFramesPerPacket: 1,
        mBytesPerFrame: 4,
        mChannelsPerFrame: 1,
        mBitsPerChannel: 32,
        mReserved: 0
    )

    do {
        var formatAddress = AudioObjectPropertyAddress(
            mSelector: kAudioTapPropertyFormat,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var formatSize = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        _ = AudioObjectGetPropertyData(
            tapID,
            &formatAddress,
            0,
            nil,
            &formatSize,
            &tapFormat
        )
    }

    let tapEntry: [String: Any] = [
        kAudioSubTapUIDKey: tapUID,
        kAudioSubTapDriftCompensationKey: true,
    ]
    let aggregateDescription: [String: Any] = [
        kAudioAggregateDeviceNameKey: "lowyap-meeting-tap",
        kAudioAggregateDeviceUIDKey: UUID().uuidString,
        kAudioAggregateDeviceIsPrivateKey: true,
        kAudioAggregateDeviceTapAutoStartKey: true,
        kAudioAggregateDeviceTapListKey: [tapEntry],
    ]

    var aggregateID = AudioObjectID(kAudioObjectUnknown)
    guard AudioHardwareCreateAggregateDevice(aggregateDescription as CFDictionary, &aggregateID) == noErr else {
        _ = AudioHardwareDestroyProcessTap(tapID)
        return nil
    }

    let context = SystemTapContext(tapID: tapID, aggregateID: aggregateID, format: tapFormat)
    var ioProcID: AudioDeviceIOProcID?
    let ioStatus = AudioDeviceCreateIOProcIDWithBlock(
        &ioProcID,
        aggregateID,
        nil
    ) { _, inInputData, _, _, _ in
        context.ingest(inInputData: inInputData)
    }
    guard ioStatus == noErr, let ioProcID else {
        context.stop()
        return nil
    }

    context.ioProcID = ioProcID
    guard AudioDeviceStart(aggregateID, ioProcID) == noErr else {
        context.stop()
        return nil
    }

    return Unmanaged.passRetained(context).toOpaque()
}

@_cdecl("lowyap_system_tap_drain")
public func lowyap_system_tap_drain(
    _ handle: UnsafeMutableRawPointer?,
    _ out_samples: UnsafeMutablePointer<Float>?,
    max_samples: Int32
) -> Int32 {
    guard let handle, max_samples > 0 else {
        return 0
    }
    guard #available(macOS 14.2, *) else {
        return 0
    }
    let context = Unmanaged<SystemTapContext>.fromOpaque(handle).takeUnretainedValue()
    return context.drain(into: out_samples, maxSamples: Int(max_samples))
}

@_cdecl("lowyap_system_tap_stop")
public func lowyap_system_tap_stop(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else { return }
    guard #available(macOS 14.2, *) else { return }

    let context = Unmanaged<SystemTapContext>.fromOpaque(handle).takeRetainedValue()
    context.stop()
}
