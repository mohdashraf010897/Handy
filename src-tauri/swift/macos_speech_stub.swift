@_cdecl("lowyap_macos_speech_available")
public func lowyapMacosSpeechAvailable(_ localePtr: UnsafePointer<CChar>?) -> Bool {
    false
}

@_cdecl("lowyap_macos_speech_auth_status")
public func lowyapMacosSpeechAuthStatus() -> Int32 { 2 }

@_cdecl("lowyap_macos_speech_request_auth")
public func lowyapMacosSpeechRequestAuth() -> Int32 { 2 }

@_cdecl("lowyap_macos_speech_transcribe_f32")
public func lowyapMacosSpeechTranscribeF32(
    _ samplesPtr: UnsafePointer<Float>, _ sampleCount: Int, _ localePtr: UnsafePointer<CChar>?,
    _ outErrorPtr: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> UnsafeMutablePointer<CChar>? {
    outErrorPtr?.pointee = nil
    return nil
}

@_cdecl("lowyap_macos_speech_free_string")
public func lowyapMacosSpeechFreeString(_ text: UnsafeMutablePointer<CChar>?) {}
