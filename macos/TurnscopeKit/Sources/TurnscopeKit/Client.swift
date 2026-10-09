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
        /// `hello`'s answer, on every connection.
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

    /// Send serve `method` with `params`, and read the answer as `R`.
    func request<R: Decodable>(_ method: String, _ params: some Encodable,
                               as _: R.Type = R.self) async throws(ClientError) -> R {
        guard phase == .connected, let connection else { throw .notConnected }
        return try decode(Answer<R>.self, from: try await connection.ask(method, params)).result
    }

    // MARK: Connecting

    /// Connect, say hello and listen. Repeat each time the connection ends,
    /// until cancelled.
    private func keepConnected() async {
        var wait = Duration.milliseconds(500)
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
                    throw .unavailable("Turnscope couldn't run its engine, \(engine.binary.path)")
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
                case .dropped:
                    // Go back to a short wait after a connection that held,
                    // but not after one that dropped as soon as it was made.
                    // That would start an engine twice a second.
                    if ContinuousClock.now - began > .seconds(30) { wait = .milliseconds(500) }
                    guard !Task.isCancelled else { return }
                    fail("Turnscope's engine stopped")
                }
            } catch {
                guard !Task.isCancelled else { return }
                fail(error.description)
            }
            do {
                try await Task.sleep(for: wait)
            } catch {
                return
            }
            wait = min(wait * 2, .seconds(30))
        }
    }

    /// How a conversation with serve ended.
    private enum Ended {
        /// Serve ran another engine, and was asked to exit.
        case replaced
        /// The connection dropped after it held.
        case dropped
    }

    /// Say hello on `connection`, then take what serve pushes until it
    /// ends. If `replacing`, a serve that names an engine other than
    /// `engine`, or doesn't speak this protocol, is asked to exit.
    private func converse(on connection: Channel, engine: String?,
                          replacing: Bool) async throws(ClientError) -> Ended {
        self.connection = connection
        let listening = Task { await connection.listen { [weak self] method, line in self?.pushed(method, line) } }
        defer {
            connection.close()
            self.connection = nil
        }
        switch try await greet(connection, engine: engine, replacing: replacing) {
        case .greeted(let hello):
            phase = .connected
            emit.yield(.hello(hello))
            await listening.value
            return .dropped
        case .another(let why):
            log.notice("replacing serve: \(why, privacy: .public)")
            _ = try? await connection.ask("shutdown", Optional<Done>.none)
            // It exits, which closes the connection. A serve that doesn't
            // exit is left to end the way serve normally does.
            let deadline = Task {
                try? await Task.sleep(for: .seconds(5))
                connection.close()
            }
            await listening.value
            deadline.cancel()
            return .replaced
        }
    }

    /// The result of `hello`.
    private enum Greeting {
        case greeted(HelloResult)
        /// Serve runs another engine or speaks another protocol, so it's to
        /// be replaced. Carries the reason.
        case another(String)
    }

    /// Say hello on `connection` and read the answer. The engine it names is
    /// read first, so a serve of an engine other than `engine` is spotted by
    /// that alone, whatever else it says, and is to be replaced if
    /// `replacing`. Then the whole answer is read.
    private func greet(_ connection: Channel, engine: String?,
                       replacing: Bool) async throws(ClientError) -> Greeting {
        struct Server: Decodable { var server: String }
        let line: Data
        do throws(ClientError) {
            line = try await connection.ask("hello", HelloParams(protocol: protocolVersion))
        } catch .unsupported(let why) where replacing {
            return .another(why)
        }
        let server = try decode(Answer<Server>.self, from: line).result.server
        if replacing, let engine, server != engine { return .another("it runs \(server), not \(engine)") }
        return .greeted(try decode(Answer<HelloResult>.self, from: line).result)
    }

    /// Handle a notification serve pushed: `method`, in `line`.
    private func pushed(_ method: String, _ line: Data) {
        struct Pushed<P: Decodable>: Decodable { var params: P }
        do throws(ClientError) {
            switch method {
            case "status": emit.yield(.status(try decode(Pushed<Status>.self, from: line).params))
            case "alert": emit.yield(.alert(try decode(Pushed<Alert>.self, from: line).params))
            default: break
            }
        } catch {
            log.error("a \(method, privacy: .public) it can't read: \(error.description, privacy: .public)")
        }
    }

    /// Record why it isn't connected. It tries again after a wait.
    private func fail(_ why: String) {
        log.notice("not connected: \(why, privacy: .public)")
        phase = .failed(why: why)
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

/// An answer, as the app reads it.
private struct Answer<R: Decodable>: Decodable {
    var result: R
}

/// Why a request has no answer, as a sentence to show.
public enum ClientError: Error, Equatable, Sendable, CustomStringConvertible {
    case notConnected
    /// The engine couldn't be run or started, and why.
    case unavailable(String)
    case closed(String)
    /// Serve answered with an error, which says why.
    case refused(String)
    /// Serve doesn't speak this app's protocol, and says which it does.
    case unsupported(String)
    case unreadable(String)
    /// A request couldn't be written for serve, and why.
    case unwritable(String)

    public var description: String {
        switch self {
        case .notConnected: "Turnscope's engine isn't running"
        case .unavailable(let why), .closed(let why), .refused(let why), .unsupported(let why): why
        case .unreadable(let why): "Turnscope couldn't read its engine's answer: \(why)"
        case .unwritable(let why): "Turnscope couldn't write a request to its engine: \(why)"
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

    /// Send `method` with `params`, and return the answer's line.
    func ask(_ method: String, _ params: some Encodable) async throws(ClientError) -> Data {
        guard !closed else { throw .closed(reason) }
        let id = nextID
        nextID += 1
        var line: Data
        do {
            line = try Contract.encoder.encode(Asked(id: id, method: method, params: params))
        } catch {
            throw .unwritable(String(describing: error))
        }
        line.append(0x0A)
        guard Channel.write(line, to: socket) else { throw .closed(reason) }
        return try await withCheckedContinuation { waiting[id] = $0 }.get()
    }

    /// Read its lines until they end. Each answer goes to the request waiting
    /// for it, and each notification to `pushed`. Then close it, and tell the
    /// requests still waiting.
    func listen(_ pushed: (String, Data) -> Void) async {
        struct Header: Decodable {
            var id: Int?
            var method: String?
            var error: Failure?
        }
        struct Failure: Decodable {
            var code: Int
            var message: String
        }
        for await line in lines {
            guard let header = try? Contract.decoder.decode(Header.self, from: line) else { continue }
            switch (header.method, header.id) {
            case (let method?, nil):
                pushed(method, line)
            case (nil, let id?):
                let answer: Result<Data, ClientError> = switch header.error {
                case let failure? where failure.code == -32001: .failure(.unsupported(failure.message))
                case let failure?: .failure(.refused(failure.message))
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
        for answer in told.values { answer.resume(returning: .failure(.closed(reason))) }
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

/// A request, as the app writes it.
private struct Asked<P: Encodable>: Encodable {
    var jsonrpc = "2.0"
    var id: Int
    var method: String
    var params: P?
}

// MARK: The engine

/// The `turnscope` binary the app runs, and the arguments it passes on.
private struct Engine: Sendable {
    var binary: URL
    var arguments: [String]

    /// A socket connected to serve, and the serve process, if one was
    /// started.
    struct Opened {
        var socket: Int32
        var serve: Process?
    }

    /// Connect to serve's socket at `path`. If nothing is listening, start
    /// serve and keep trying for five seconds while it starts.
    nonisolated func open(_ path: String) async throws(ClientError) -> sending Opened {
        if let socket = Engine.connect(to: path) { return Opened(socket: socket, serve: nil) }
        let serve = Process()
        serve.executableURL = binary
        serve.arguments = ["serve"] + arguments
        serve.standardInput = FileHandle.nullDevice
        serve.standardOutput = FileHandle.nullDevice
        serve.standardError = FileHandle.nullDevice
        do {
            try serve.run()
        } catch {
            throw .unavailable("Turnscope couldn't start its engine: \(error.localizedDescription)")
        }
        // A serve that exits at once, as a second one does when another is
        // already running, is dealt with on the next try.
        for _ in 0..<50 {
            do {
                try await Task.sleep(for: .milliseconds(100))
            } catch {
                throw .closed("Turnscope stopped")
            }
            if let socket = Engine.connect(to: path) { return Opened(socket: socket, serve: serve) }
        }
        throw .unavailable("Turnscope's engine didn't start")
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
