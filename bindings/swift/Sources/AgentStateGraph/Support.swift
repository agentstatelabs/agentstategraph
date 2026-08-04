import Foundation
import CAgentStateGraph

/// Errors thrown by the AgentStateGraph Swift binding.
public enum AgentStateGraphError: Error, CustomStringConvertible {
    /// A native call returned NULL — the operation failed with no message
    /// (e.g. key not found, bad handle, invalid input). Carries the
    /// operation name for context.
    case operationFailed(String)
    /// The native layer returned a structured error string, either
    /// `{"error":"…"}` JSON or an `error:…` prefixed message.
    case native(String)
    /// A returned JSON payload could not be decoded into the expected type.
    case decode(String)
    /// A handle was used after it had been closed/freed.
    case closed(String)

    public var description: String {
        switch self {
        case .operationFailed(let op): return "AgentStateGraph: \(op) failed"
        case .native(let msg): return "AgentStateGraph: \(msg)"
        case .decode(let msg): return "AgentStateGraph: decode: \(msg)"
        case .closed(let what): return "AgentStateGraph: \(what) is closed"
        }
    }
}

/// Duplicate a Swift string into a C string the callee copies from.
/// Returns `nil` for a `nil` input (used for the many `*_or_null` params).
@inline(__always)
func sgDup(_ s: String?) -> UnsafeMutablePointer<CChar>? {
    guard let s = s else { return nil }
    return strdup(s)
}

/// Take ownership of a `char*` returned by the FFI: convert to a Swift
/// String, free the native buffer, and translate the null / error-string
/// conventions into thrown errors.
///
/// The C ABI signals failure three ways, all handled here:
///   • NULL pointer            → `.operationFailed(op)`
///   • `{"error":"…"}` JSON     → `.native(payload)`
///   • `error:…` prefix (merge) → `.native(payload)`
@inline(__always)
func consume(_ ptr: UnsafeMutablePointer<CChar>?, _ op: String) throws -> String {
    guard let ptr = ptr else { throw AgentStateGraphError.operationFailed(op) }
    defer { agentstategraph_free_string(ptr) }
    let s = String(cString: ptr)
    if s.hasPrefix("{\"error\"") || s.hasPrefix("error:") {
        throw AgentStateGraphError.native(s)
    }
    return s
}

/// Decode a JSON string returned by the FFI into a `Decodable` value.
@inline(__always)
func decodeJSON<T: Decodable>(_ json: String, as type: T.Type = T.self) throws -> T {
    guard let data = json.data(using: .utf8) else {
        throw AgentStateGraphError.decode("not utf-8")
    }
    do {
        return try JSONDecoder().decode(T.self, from: data)
    } catch {
        throw AgentStateGraphError.decode("\(error): \(json)")
    }
}

/// Encode any `Encodable` value to a compact JSON string for the FFI.
@inline(__always)
func encodeJSON<T: Encodable>(_ value: T) throws -> String {
    let data = try JSONEncoder().encode(value)
    guard let s = String(data: data, encoding: .utf8) else {
        throw AgentStateGraphError.decode("encode: not utf-8")
    }
    return s
}

/// A type-erased JSON value, used for schema-flexible fields the binding
/// does not prescribe a shape for (e.g. a task `payload` or `on_complete`
/// hook). Round-trips arbitrary JSON without loss.
public enum JSONValue: Codable, Sendable, Equatable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let b = try? c.decode(Bool.self) {
            self = .bool(b)
        } else if let n = try? c.decode(Double.self) {
            self = .number(n)
        } else if let s = try? c.decode(String.self) {
            self = .string(s)
        } else if let a = try? c.decode([JSONValue].self) {
            self = .array(a)
        } else if let o = try? c.decode([String: JSONValue].self) {
            self = .object(o)
        } else {
            throw DecodingError.dataCorruptedError(
                in: c, debugDescription: "unrecognized JSON value")
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        case .number(let n): try c.encode(n)
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }
}
