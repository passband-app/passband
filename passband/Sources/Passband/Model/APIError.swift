// Central error type for the human-door client. Every non-2xx response becomes
// an APIError; callers switch on `kind` rather than sniffing status codes.

import Foundation

enum APIErrorKind: String, Sendable {
    case unauthorized   // 401: bad/absent bearer token
    case forbidden      // 403: no write credential (run `squelchd auth`)
    case notFound       // 404: missing OR sealed-hidden (indistinguishable by design)
    case badRequest     // 400: validation
    case server         // 5xx
    case network        // transport failure / timeout
    case unknown

    static func forStatus(_ status: Int) -> APIErrorKind {
        switch status {
        case 400: .badRequest
        case 401: .unauthorized
        case 403: .forbidden
        case 404: .notFound
        default: status >= 500 ? .server : .unknown
        }
    }
}

struct APIError: Error, Sendable {
    var kind: APIErrorKind
    var status: Int
    var message: String
    /// A transport failure after which the server may still have received
    /// the request and acted on it: the wait timed out, the connection
    /// dropped mid-request, or an answer came back that could not be read
    /// (a bad or empty response: the server got the request). False for one
    /// that never got there (refused, no route, bad address). Only ever true
    /// with `kind == .network`.
    var mayHaveArrived = false
    /// The daemon's machine-readable name for the refusal, the body's
    /// `code`, where the route serves one (the calendar writes:
    /// `CalendarRefusal`). Nil when the body had none, or was not JSON.
    var code: String?

    init(
        _ kind: APIErrorKind, _ status: Int, _ message: String, code: String? = nil
    ) {
        self.kind = kind
        self.status = status
        self.message = message
        self.code = code
    }

    /// A transport failure as the client reports it: never the URL or the
    /// token, and whether the request may have reached the server.
    ///
    /// `evidence` is what the task itself recorded about the send, when the
    /// caller has it (`APIClient.perform` does): it decides before the
    /// error's code is even read (`neverSent`).
    static func transport(_ error: Error, evidence: SendEvidence? = nil) -> APIError {
        var out = APIError(.network, 0, "cannot reach squelch server")
        out.mayHaveArrived = !neverSent(code: (error as? URLError)?.code, evidence: evidence)
        return out
    }
}

/// What a failed request's own task recorded about whether anything left
/// for the server. Each half is nil when it was not collected.
struct SendEvidence: Hashable, Sendable {
    /// From the task's metrics: whether ANY transaction (a redirect or a
    /// retry counts) began sending its request. Nil with no metrics.
    var requestStarted: Bool?
    /// The task's `countOfBytesSent`: bytes of request BODY handed to the
    /// connection. Zero proves nothing for a request with no body.
    var bodyBytesSent: Int64?
    /// Whether the request had a body to send at all.
    var hadBody = false
}

extension APIError {
    /// WHETHER A FAILED REQUEST PROVABLY NEVER REACHED THE SERVER (verify6
    /// Z1). By EVIDENCE first, by the error's code only when there is none:
    ///
    ///   1. Anything that says the request went out wins: a transaction that
    ///      started its request, or body bytes sent. Then it may have
    ///      arrived, whatever the code (a "not connected" raised on a
    ///      connection that dropped mid-request is exactly this).
    ///   2. Metrics that show no transaction ever started a request are
    ///      proof it was not sent, whatever the code (a timeout while still
    ///      connecting; a true offline save, which keeps its friendly
    ///      message).
    ///   3. With no metrics, a request that HAD a body and sent none of it
    ///      was not sent. A bodyless one proves nothing that way.
    ///   4. Otherwise the code list: only failures that cannot follow a
    ///      send. `.notConnectedToInternet` is NOT on it: CFNetwork reports
    ///      an interface that went away under a live request the same way.
    static func neverSent(code: URLError.Code?, evidence: SendEvidence?) -> Bool {
        if let evidence {
            if evidence.requestStarted == true || (evidence.bodyBytesSent ?? 0) > 0 { return false }
            if evidence.requestStarted == false { return true }
            if evidence.hadBody, evidence.bodyBytesSent == 0 { return true }
        }
        return code.map(notSentCodes.contains) ?? false
    }
}

extension APIError {
    /// The fallback, used only when the task left no evidence (`neverSent`).
    /// The transport failures that provably happen BEFORE a request is sent:
    /// no host, no connection, a TLS handshake refused. Every
    /// other failure (a timeout, a dropped connection, an answer that did
    /// not read, a cancellation, anything unknown, any error that is not a
    /// URLError) may have followed the request reaching the server, so a
    /// calendar write that ends there freezes its pane (verify5 Y2: when in
    /// doubt, freeze).
    static let notSentCodes: Set<URLError.Code> = [
        .badURL, .unsupportedURL, .cannotFindHost, .cannotConnectToHost, .dnsLookupFailed,
        .appTransportSecurityRequiresSecureConnection, .secureConnectionFailed,
        .serverCertificateUntrusted, .serverCertificateHasBadDate, .serverCertificateNotYetValid,
        .serverCertificateHasUnknownRoot, .clientCertificateRejected, .clientCertificateRequired,
    ]
}

extension APIError: LocalizedError {
    var errorDescription: String? { message }
}

/// Human text for any thrown error, without ever echoing a token or URL.
func errText(_ error: Error, _ fallback: String) -> String {
    if let api = error as? APIError { return api.message }
    return fallback
}

/// A temporary outage is not evidence of an incompatible daemon. Keep retries
/// bounded so a switch cannot hold the account picker indefinitely.
struct CapabilityProbeRetry {
    private var failures = 0
    private static let delays: [TimeInterval] = [1, 3, 10]

    static func isTransient(_ error: Error) -> Bool {
        guard let error = error as? APIError else { return false }
        return error.kind == .network || error.kind == .server
    }

    mutating func delay(after error: Error) -> TimeInterval? {
        guard Self.isTransient(error), failures < Self.delays.count else { return nil }
        defer { failures += 1 }
        return Self.delays[failures]
    }
}
