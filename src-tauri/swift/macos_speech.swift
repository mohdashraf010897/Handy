import AVFoundation
import Foundation
import Speech

private let sampleRate = 16_000
private let windowFrames = 45 * sampleRate
private let usageDescriptionKey = "NSSpeechRecognitionUsageDescription"

private func localRecognizer(_ localePtr: UnsafePointer<CChar>?) -> SFSpeechRecognizer? {
    let current = Locale.current
    let requested = localePtr.map { String(cString: $0) }
    let locale = requested.map { Locale(identifier: $0) } ?? current
    let supported = SFSpeechRecognizer.supportedLocales()
    func supportedMatch(_ locale: Locale) -> Locale? {
        let tag = locale.identifier.replacingOccurrences(of: "_", with: "-").lowercased()
        return supported.first {
            $0.identifier.replacingOccurrences(of: "_", with: "-").lowercased() == tag
        }
    }
    var candidates = supportedMatch(locale).map { [$0] } ?? []
    // The app normally stores a language, not a region. Keep explicit
    // region/script tags exact and only look for variants of a bare language.
    if let requested, !requested.contains("-"), !requested.contains("_"),
       let language = locale.languageCode {
        if current.languageCode == language, let current = supportedMatch(current) {
            candidates.append(current)
        }
        candidates.append(contentsOf: supported
            .filter { $0.languageCode == language }
            .sorted { $0.identifier < $1.identifier })
    }
    for candidate in candidates {
        if let recognizer = SFSpeechRecognizer(locale: candidate),
           recognizer.supportsOnDeviceRecognition, recognizer.isAvailable {
            return recognizer
        }
    }
    return nil
}

@_cdecl("lowyap_macos_speech_available")
public func lowyapMacosSpeechAvailable(_ localePtr: UnsafePointer<CChar>?) -> Bool {
    localRecognizer(localePtr) != nil
}

@_cdecl("lowyap_macos_speech_auth_status")
public func lowyapMacosSpeechAuthStatus() -> Int32 {
    switch SFSpeechRecognizer.authorizationStatus() {
    case .notDetermined: return 0
    case .denied: return 1
    case .restricted: return 2
    case .authorized: return 3
    @unknown default: return 2
    }
}

@_cdecl("lowyap_macos_speech_request_auth")
public func lowyapMacosSpeechRequestAuth() -> Int32 {
    let status = lowyapMacosSpeechAuthStatus()
    // Never block the main thread while waiting for the system prompt.
    guard status == 0, !Thread.isMainThread,
          Bundle.main.object(forInfoDictionaryKey: usageDescriptionKey) is String else { return status }
    let completed = DispatchSemaphore(value: 0)
    DispatchQueue.global(qos: .userInitiated).async {
        SFSpeechRecognizer.requestAuthorization { _ in completed.signal() }
    }
    _ = completed.wait(timeout: .now() + 15)
    return lowyapMacosSpeechAuthStatus()
}

private enum SpeechFailure: String, Error {
    case timedOut = "On-device macOS Speech Recognition timed out."
    case unavailable = "On-device macOS Speech Recognition is unavailable for this language. Check your macOS speech language settings or choose a downloaded model."
    case failed = "On-device macOS Speech Recognition could not transcribe this recording."
    case buffer = "Could not prepare audio for on-device macOS Speech Recognition."
}

private final class RecognitionReply {
    private let lock = NSLock()
    private var result: Result<String, SpeechFailure>?
    private let ready = DispatchSemaphore(value: 0)

    func complete(_ value: Result<String, SpeechFailure>) {
        lock.lock()
        defer { lock.unlock() }
        guard result == nil else { return }
        result = value
        ready.signal()
    }

    func wait(seconds: TimeInterval) throws -> String {
        guard ready.wait(timeout: .now() + seconds) == .success else {
            throw SpeechFailure.timedOut
        }
        lock.lock()
        defer { lock.unlock() }
        return try (result ?? .failure(.failed)).get()
    }
}

private func transcribeWindow(
    _ samples: UnsafeBufferPointer<Float>, format: AVAudioFormat, recognizer: SFSpeechRecognizer
) throws -> String {
    // Apple only honors requiresOnDeviceRecognition when this is true.
    guard recognizer.supportsOnDeviceRecognition, recognizer.isAvailable else {
        throw SpeechFailure.unavailable
    }
    guard let source = samples.baseAddress,
          let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(samples.count)),
          let channel = buffer.floatChannelData?[0] else { throw SpeechFailure.buffer }
    buffer.frameLength = AVAudioFrameCount(samples.count)
    channel.update(from: source, count: samples.count)

    let request = SFSpeechAudioBufferRecognitionRequest()
    request.requiresOnDeviceRecognition = true
    request.shouldReportPartialResults = false
    if #available(macOS 13.0, *) { request.addsPunctuation = true }

    let reply = RecognitionReply()
    let task = recognizer.recognitionTask(with: request) { result, error in
        if let result, result.isFinal {
            reply.complete(.success(result.bestTranscription.formattedString))
        } else if let error = error as NSError? {
            // Apple's “No speech detected” is a clean empty transcript. Never
            // forward Apple's error strings: they may contain user data.
            let noSpeech = error.domain == "kAFAssistantErrorDomain" && error.code == 1110
            reply.complete(noSpeech ? .success("") : .failure(.failed))
        }
    }
    defer { task.cancel() }
    request.append(buffer)
    request.endAudio()
    return try reply.wait(seconds: max(12, Double(samples.count) / Double(sampleRate) * 1.5))
}

@_cdecl("lowyap_macos_speech_transcribe_f32")
public func lowyapMacosSpeechTranscribeF32(
    _ samplesPtr: UnsafePointer<Float>, _ sampleCount: Int, _ localePtr: UnsafePointer<CChar>?,
    _ outErrorPtr: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> UnsafeMutablePointer<CChar>? {
    outErrorPtr?.pointee = nil
    func fail(_ message: String) -> UnsafeMutablePointer<CChar>? {
        outErrorPtr?.pointee = strdup(message)
        return nil
    }
    guard sampleCount >= 0 else { return fail(SpeechFailure.buffer.rawValue) }
    let samples = UnsafeBufferPointer(start: samplesPtr, count: sampleCount)
    guard samples.contains(where: { $0 != 0 }) else { return strdup("") }
    guard let recognizer = localRecognizer(localePtr) else {
        return fail(SpeechFailure.unavailable.rawValue)
    }
    // The OS terminates an unbundled process that requests authorization
    // without a usage description; return a local error instead.
    if lowyapMacosSpeechAuthStatus() == 0,
       !(Bundle.main.object(forInfoDictionaryKey: usageDescriptionKey) is String) {
        return fail("Open the lowyap app bundle to grant macOS Speech Recognition permission.")
    }
    switch lowyapMacosSpeechRequestAuth() {
    case 3: break
    case 1: return fail("Allow lowyap under System Settings → Privacy & Security → Speech Recognition.")
    case 2: return fail("On-device macOS Speech Recognition permission is restricted.")
    default: return fail("On-device macOS Speech Recognition is waiting for permission. Allow access and try again.")
    }

    guard let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Double(sampleRate),
                                     channels: 1, interleaved: false) else {
        return fail(SpeechFailure.buffer.rawValue)
    }
    let callbackQueue = OperationQueue()
    callbackQueue.maxConcurrentOperationCount = 1
    callbackQueue.qualityOfService = .userInitiated
    recognizer.queue = callbackQueue

    do {
        var transcripts: [String] = []
        for offset in stride(from: 0, to: sampleCount, by: windowFrames) {
            let window = UnsafeBufferPointer(start: samplesPtr.advanced(by: offset),
                                             count: min(windowFrames, sampleCount - offset))
            if !window.contains(where: { $0 != 0 }) { continue }
            let text = try transcribeWindow(window, format: format, recognizer: recognizer)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if !text.isEmpty { transcripts.append(text) }
        }
        return strdup(transcripts.joined(separator: " "))
    } catch let error as SpeechFailure {
        return fail(error.rawValue)
    } catch {
        return fail(SpeechFailure.failed.rawValue)
    }
}

@_cdecl("lowyap_macos_speech_free_string")
public func lowyapMacosSpeechFreeString(_ text: UnsafeMutablePointer<CChar>?) {
    free(text)
}
