// The app's connection to `turnscope serve`: JSON-RPC 2.0 over a Unix socket,
// one message per line (docs/protocol.md).
//
// Serve gives the socket path (`turnscope serve --socket`); the app never
// works it out. When nothing is listening, the client starts `turnscope
// serve`. Serve outlives the app and exits a minute after its last client
// leaves, so a restarted app finds it still running. Each connection starts
// with `hello`, which returns the whole status and the alerts not yet
// acknowledged, so a new connection is in sync at once. After that, serve
// pushes `status` and `alert`.
//
// The app only talks to the engine it ships with. After an update, the old
// serve may still be running, with another engine, protocol or status format.
// `hello` names the engine first; if it isn't this app's, the client asks it
// to exit, once, and starts this app's own.
//
// A dropped connection is made again after half a second, then twice as long
// each time up to thirty. The wait goes back to half a second only after a
// connection has held for thirty seconds, so an engine that keeps dropping
// isn't started twice a second.

import Darwin
import Foundation
import os

@MainActor
public final class Client {
    public enum Phase: Equatable, Sendable {
        case connecting
        case connected
        /// It isn't connected, and why. It tries again on its own.
        case failed(why: String)
    }

    /// What the client hears, in order.
    public enum Event: Sendable {
        /// It entered a phase.
        case phase(Phase)
        /// `hello`'s answer, on every connection: its status and alerts
        /// handled as one.
        case hello(HelloResult)
        /// A status serve pushed.
        case status(Status)
        /// An alert serve pushed.
        case alert(Alert)
    }

    public private(set) var phase: Phase = .connecting {
        didSet { if phase != oldValue { emit.yield(.phase(phase)) } }
    }

    /// What the client hears, for a single listener (`Store`).
    public let events: AsyncStream<Event>
    private let emit: AsyncStream<Event>.Continuation

    private let engine: Engine
    private let log = Logger(subsystem: "com.joeychilson.turnscope", category: "serve")
    /// The task that keeps it connected, from `start` until `stop`.
    private var keeping: Task<Void, Never>?
    /// The current connection, if there is one.
    private var connection: Channel?
    /// The `serve` processes it started, kept until each ends.
    private var serves: [Process] = []
    /// Serve's socket path and the engine's name, once the binary has said
    /// them. Neither changes while the app runs, so neither is asked again
    /// on each retry.
    private var socketPath: String?
    private var engineName: String?
    /// How long serve has to answer `hello`. It works out a whole status for
    /// that, which is slow on a long history. Tests shorten it.
    var helloWait = Duration.seconds(30)
    /// How long another engine's serve has to exit once asked to. It only
    /// has to end its process. Tests shorten it.
    var exitWait = Duration.seconds(5)

    /// A client of the serve run by `binary` (`turnscope`), with
    /// `arguments` (`--data`, `--home`) passed on.
    public init(binary: URL, arguments: [String] = []) {
        engine = Engine(binary: binary, arguments: arguments)
        (events, emit) = AsyncStream.makeStream(of: Event.self)
    }

    /// Connect, and stay connected until `stop`.
    public func start() {
        guard keeping == nil else { return }
        keeping = Task { await keepConnected() }
    }

    /// Disconnect. Serve keeps running, and exits a minute after its last
    /// client leaves.
    public func stop() {
        keeping?.cancel()
        keeping = nil
        connection?.close()
    }

    // MARK: Requests

    /// Send serve `method` with `params`, if it takes any, and read the
    /// answer as `R`.
    func request<R: Decodable>(_ method: String, _ params: (any Encodable)? = nil,
                               as _: R.Type = R.self) async throws(ClientError) -> R {
        guard phase == .connected, let connection else { throw .notConnected }
        return try decode(Answer<R>.self, from: try await connection.ask(method, params)).result
    }

    // MARK: Connecting

    /// Connect, say hello and listen. Repeat each time the connection ends,
    /// until cancelled.
    private func keepConnected() async {
        var backoff = Backoff()
        // A serve of another engine is asked to exit once. The one after it
        // is kept, whatever engine it runs, as long as what it says can be
        // read. That way two apps of different versions can't take turns
        // ending each other's serve.
        var replaced = false
        while !Task.isCancelled {
            if phase == .connected { phase = .connecting }
            do throws(ClientError) {
                // Serve decides the socket's path.
                if socketPath == nil { socketPath = await engine.said(["serve", "--socket"] + engine.arguments) }
                guard let path = socketPath else {
                    throw .failed("Turnscope couldn't run its engine, \(engine.binary.path)")
                }
                if engineName == nil { engineName = await engine.said(["--version"]) }
                let opened = try await engine.open(path)
                if let serve = opened.serve { serves.append(serve) }
                serves.removeAll { !$0.isRunning }
                let began = ContinuousClock.now
                let ended = try await converse(on: Channel(socket: opened.socket), engine: engineName,
                                               replacing: !replaced)
                switch ended {
                case .replaced:
                    replaced = true
                    continue
                case .dropped(let why):
                    backoff.ended(after: ContinuousClock.now - began)
                    guard !Task.isCancelled else { return }
                    fail(why)
                }
            } catch {
                guard !Task.isCancelled else { return }
                fail(error.description)
            }
            do {
                try await Task.sleep(for: backoff.next())
            } catch {
                return
            }
        }
    }

    /// How a conversation with serve ended.
    private enum Ended {
        /// Serve ran another engine, and was asked to exit.
        case replaced
        /// The connection dropped after it held, and why.
        case dropped(why: String)
    }

    /// Say hello on `connection`, then take what serve pushes until it
    /// ends. If `replacing`, a serve that names an engine other than
    /// `engine`, or doesn't speak this protocol, is asked to exit.
    private func converse(on connection: Channel, engine: String?,
                          replacing: Bool) async throws(ClientError) -> Ended {
        self.connection = connection
        let listening = Task {
            await connection.listen { [weak self] method, line throws(ClientError) in try self?.pushed(method, line) }
        }
        defer {
            connection.close()
            self.connection = nil
        }
        let hello: HelloResult
        do throws(ClientError) {
            hello = try await greet(connection, engine: engine, replacing: replacing)
        } catch .unsupported(let why) where replacing {
            log.notice("replacing serve: \(why, privacy: .public)")
            // It answers and exits, which closes the connection. A serve that
            // does neither is left to end the way serve normally does.
            let deadline = connection.end(after: exitWait)
            _ = try? await connection.ask("shutdown")
            _ = await listening.value
            deadline.cancel()
            return .replaced
        }
        phase = .connected
        emit.yield(.hello(hello))
        let unread = await listening.value
        return .dropped(why: unread?.description ?? "Turnscope's engine stopped")
    }

    /// Say hello on `connection` and read the answer. The engine it names is
    /// read first, so a serve of an engine other than `engine` is spotted by
    /// that alone, whatever else it says, and if `replacing` is
    /// `unsupported`, like one that speaks another protocol. Then the whole
    /// answer is read.
    private func greet(_ connection: Channel, engine: String?,
                       replacing: Bool) async throws(ClientError) -> HelloResult {
        struct Server: Decodable { var server: String }
        let deadline = connection.end(after: helloWait)
        defer { deadline.cancel() }
        let line = try await connection.ask("hello", HelloParams(protocol: protocolVersion))
        let server = try decode(Answer<Server>.self, from: line).result.server
        if replacing, let engine, server != engine { throw .unsupported("it runs \(server), not \(engine)") }
        return try decode(Answer<HelloResult>.self, from: line).result
    }

    /// Handle a notification serve pushed: `method`, in `line`.
    private func pushed(_ method: String, _ line: Data) throws(ClientError) {
        switch method {
        case "status": emit.yield(.status(try decode(Pushed<Status>.self, from: line).params))
        case "alert": emit.yield(.alert(try decode(Pushed<Alert>.self, from: line).params))
        default: break
        }
    }

    /// Record why it isn't connected. It tries again after a wait.
    private func fail(_ why: String) {
        log.notice("not connected: \(why, privacy: .public)")
        phase = .failed(why: why)
    }
}

/// How long to wait before connecting again, as the top of this file says.
struct Backoff {
    private var wait = Duration.milliseconds(500)

    /// A connection ended after it held for `held`.
    mutating func ended(after held: Duration) {
        if held > .seconds(30) { wait = .milliseconds(500) }
    }

    /// The wait before the next try.
    mutating func next() -> Duration {
        defer { wait = min(wait * 2, .seconds(30)) }
        return wait
    }
}

/// `line` decoded as `T`.
private func decode<T: Decodable>(_: T.Type, from line: Data) throws(ClientError) -> T {
    do {
        return try Contract.decoder.decode(T.self, from: line)
    } catch {
        throw .unreadable(String(describing: error))
    }
}

/// Why a request has no answer, as a sentence to show.
public enum ClientError: Error, Equatable, Sendable, CustomStringConvertible {
    case notConnected
    /// The engine couldn't be run or started, the connection closed, or
    /// serve answered with an error, and why.
    case failed(String)
    /// Serve doesn't speak this app's protocol, and says which it does, or
    /// runs another engine.
    case unsupported(String)
    case unreadable(String)

    public var description: String {
        switch self {
        case .notConnected: "Turnscope's engine isn't running"
        case .failed(let why), .unsupported(let why): why
        case .unreadable(let why): "Turnscope couldn't read what its engine sent: \(why)"
        }
    }
}

// MARK: The connection

/// One connection to serve: its socket, the lines read from it, and the
/// requests waiting for answers.
///
/// The socket is closed here, once its lines have ended, and never while the
/// thread reading it may still read.
@MainActor
private final class Channel {
    private let socket: Int32
    private let lines: AsyncStream<Data>
    private var nextID = 1
    private var waiting: [Int: CheckedContinuation<Result<Data, ClientError>, Never>] = [:]
    /// Whether it was closed or its lines ended. If so, nothing more is
    /// written.
    private var closed = false
    /// Why it ended, as told to requests still waiting.
    private var reason = "the connection to Turnscope's engine closed"

    init(socket: Int32) {
        self.socket = socket
        lines = Channel.lines(of: socket)
    }

    /// Send `method` with `params`, if it takes any, and return the answer's
    /// line.
    func ask(_ method: String, _ params: (any Encodable)? = nil) async throws(ClientError) -> Data {
        guard !closed else { throw .failed(reason) }
        let id = nextID
        nextID += 1
        let line = Asked(id: id, method: method, params: params).line()
        guard Channel.write(line, to: socket) else { throw .failed(reason) }
        return try await withCheckedContinuation { waiting[id] = $0 }.get()
    }

    /// Read its lines until they end. Each answer goes to the request waiting
    /// for it, and each notification to `pushed`. Then close it, and tell the
    /// requests still waiting. A notification `pushed` can't read closes it
    /// too, since what's shown is then out of date, and is returned.
    func listen(_ pushed: (String, Data) throws(ClientError) -> Void) async -> ClientError? {
        struct Header: Decodable {
            var id: Int?
            var method: String?
            var error: Failure?
        }
        struct Failure: Decodable {
            var code: Int
            var message: String
        }
        var unread: ClientError?
        for await line in lines {
            guard let header = try? Contract.decoder.decode(Header.self, from: line) else { continue }
            switch (header.method, header.id) {
            // Those after it are dropped with the connection.
            case (let method?, nil) where unread == nil:
                do throws(ClientError) {
                    try pushed(method, line)
                } catch {
                    unread = error
                    close()
                }
            case (nil, let id?):
                let answer: Result<Data, ClientError> = switch header.error {
                case let failure? where failure.code == -32001: .failure(.unsupported(failure.message))
                case let failure?: .failure(.failed(failure.message))
                case nil: .success(line)
                }
                waiting.removeValue(forKey: id)?.resume(returning: answer)
            default: break
            }
        }
        close()
        Darwin.close(socket)
        let told = waiting
        waiting = [:]
        for answer in told.values { answer.resume(returning: .failure(.failed(reason))) }
        return unread
    }

    /// End it after `wait` unless the task this returns is cancelled first,
    /// for a serve that's stuck. Requests still waiting are told it didn't
    /// answer.
    func end(after wait: Duration) -> Task<Void, Never> {
        Task {
            do {
                try await Task.sleep(for: wait)
            } catch {
                return
            }
            reason = "Turnscope's engine didn't answer"
            close()
        }
    }

    /// End it. Its lines end, and `listen` closes the socket.
    func close() {
        guard !closed else { return }
        closed = true
        Darwin.shutdown(socket, SHUT_RDWR)
    }

    /// The lines read from `socket`, on its own thread, until it closes.
    nonisolated private static func lines(of socket: Int32) -> AsyncStream<Data> {
        AsyncStream { lines in
            let reader = Thread {
                var buffer = Data()
                var chunk = [UInt8](repeating: 0, count: 65_536)
                while true {
                    let count = Darwin.read(socket, &chunk, chunk.count)
                    if count < 0 && errno == EINTR { continue }
                    if count <= 0 { break }
                    buffer.append(contentsOf: chunk[..<count])
                    while let end = buffer.firstIndex(of: 0x0A) {
                        let line = buffer[..<end]
                        buffer.removeSubrange(...end)
                        if !line.isEmpty { lines.yield(Data(line)) }
                    }
                }
                lines.finish()
            }
            reader.name = "turnscope-serve"
            reader.start()
        }
    }

    /// Write all of `data` to `socket`. Returns whether it could.
    private static func write(_ data: Data, to socket: Int32) -> Bool {
        data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return true }
            var sent = 0
            while sent < raw.count {
                let count = Darwin.write(socket, base + sent, raw.count - sent)
                if count < 0 {
                    if errno == EINTR { continue }
                    return false
                }
                sent += count
            }
            return true
        }
    }
}

/// A request, as the app writes it. One with no parameters has no `params`,
/// since JSON-RPC 2.0 allows only an object or an array there.
struct Asked: Encodable {
    var id: Int
    var method: String
    var params: (any Encodable)?

    private enum Keys: String, CodingKey { case jsonrpc, id, method, params }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Keys.self)
        try container.encode("2.0", forKey: .jsonrpc)
        try container.encode(id, forKey: .id)
        try container.encode(method, forKey: .method)
        if let params { try container.encode(params, forKey: .params) }
    }

    /// The line written to serve.
    func line() -> Data {
        // It can't fail: the only parameters are the app's own, which hold
        // whole numbers, strings and booleans.
        var line = try! JSONEncoder().encode(self)
        line.append(0x0A)
        return line
    }
}

// MARK: The engine

/// The `turnscope` binary the app runs, and the arguments it passes on.
private struct Engine: Sendable {
    var binary: URL
    var arguments: [String]

    /// Connect to serve's socket at `path`. If nothing is listening, start
    /// serve and keep trying for five seconds while it starts. Returns the
    /// socket, and the serve process if it started one.
    nonisolated func open(_ path: String) async throws(ClientError) -> sending (socket: Int32, serve: Process?) {
        if let socket = Engine.connect(to: path) { return (socket, nil) }
        let serve = Process()
        serve.executableURL = binary
        serve.arguments = ["serve"] + arguments
        serve.standardInput = FileHandle.nullDevice
        serve.standardOutput = FileHandle.nullDevice
        serve.standardError = FileHandle.nullDevice
        do {
            try serve.run()
        } catch {
            throw .failed("Turnscope couldn't start its engine: \(error.localizedDescription)")
        }
        // A serve that exits at once, as a second one does when another is
        // already running, is dealt with on the next try.
        for _ in 0..<50 {
            do {
                try await Task.sleep(for: .milliseconds(100))
            } catch {
                throw .failed("Turnscope stopped")
            }
            if let socket = Engine.connect(to: path) { return (socket, serve) }
        }
        throw .failed("Turnscope's engine didn't start")
    }

    /// The line the binary prints when run with `arguments`, if it succeeds:
    /// the socket's path for `serve --socket`, or for `--version` the engine
    /// it is, `turnscope 0.1.0`, as `hello` names it.
    nonisolated func said(_ arguments: [String]) async -> String? {
        let process = Process()
        process.executableURL = binary
        process.arguments = arguments
        let out = Pipe()
        process.standardOutput = out
        process.standardError = FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        let status: Int32? = await withCheckedContinuation { ended in
            process.terminationHandler = { ended.resume(returning: $0.terminationStatus) }
            do {
                try process.run()
            } catch {
                process.terminationHandler = nil
                ended.resume(returning: nil)
            }
        }
        // A single line, which the pipe holds in full while the process ends.
        guard status == 0, let data = try? out.fileHandleForReading.readToEnd() else { return nil }
        let line = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        return line.isEmpty ? nil : line
    }

    /// A socket connected to `path`, or nil if nothing is listening there.
    nonisolated static func connect(to path: String) -> Int32? {
        var address = sockaddr_un()
        let bytes = Array(path.utf8) + [0]
        guard bytes.count <= MemoryLayout.size(ofValue: address.sun_path) else { return nil }
        address.sun_family = sa_family_t(AF_UNIX)
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        withUnsafeMutableBytes(of: &address.sun_path) { $0.copyBytes(from: bytes) }
        let socket = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard socket >= 0 else { return nil }
        // A write to a socket that serve closed returns an error instead of
        // ending the app.
        var on: Int32 = 1
        setsockopt(socket, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
        let connected = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(socket, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else {
            Darwin.close(socket)
            return nil
        }
        return socket
    }
}
