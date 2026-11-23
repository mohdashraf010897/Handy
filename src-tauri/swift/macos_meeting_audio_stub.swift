import Foundation

@_cdecl("lowyap_is_default_input_running")
public func lowyap_is_default_input_running() -> Int32 {
    0
}

@_cdecl("lowyap_detect_output_route")
public func lowyap_detect_output_route() -> Int32 {
    0
}

@_cdecl("lowyap_detect_running_call_app")
public func lowyap_detect_running_call_app(
    _ out_bundle: UnsafeMutablePointer<CChar>?,
    bundle_cap: Int32,
    _ out_name: UnsafeMutablePointer<CChar>?,
    name_cap: Int32
) -> Int32 {
    if let out_bundle, bundle_cap > 0 {
        out_bundle[0] = 0
    }
    if let out_name, name_cap > 0 {
        out_name[0] = 0
    }
    return 0
}

@_cdecl("lowyap_system_tap_start")
public func lowyap_system_tap_start() -> UnsafeMutableRawPointer? {
    nil
}

@_cdecl("lowyap_system_tap_drain")
public func lowyap_system_tap_drain(
    _ handle: UnsafeMutableRawPointer?,
    _ out_samples: UnsafeMutablePointer<Float>?,
    max_samples: Int32
) -> Int32 {
    0
}

@_cdecl("lowyap_system_tap_stop")
public func lowyap_system_tap_stop(_ handle: UnsafeMutableRawPointer?) {}

@_cdecl("lowyap_calendar_auth_status")
public func lowyap_calendar_auth_status() -> Int32 {
    2
}

@_cdecl("lowyap_request_calendar_access")
public func lowyap_request_calendar_access() -> Int32 {
    2
}

@_cdecl("lowyap_active_calendar_meeting_json")
public func lowyap_active_calendar_meeting_json(
    _ out_json: UnsafeMutablePointer<CChar>?,
    cap: Int32
) -> Int32 {
    if let out_json, cap > 0 {
        out_json[0] = 0
    }
    return 0
}
