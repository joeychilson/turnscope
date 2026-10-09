// What the kit's tests share: the contract's files, and a stand-in for
// `turnscope serve`: a Unix socket that answers as serve does, and a
// script that says where it is, as `turnscope serve --socket` does.

import Darwin
import Foundation
import Synchronization

/// The repository's root, five folders up from this file.
private let root = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    .deletingLastPathComponent().deletingLastPathComponent()

/// The file `name` in the repository's `contract/` folder.
func contract(_ name: String) throws -> Data {
    try Data(contentsOf: root.appending(path: "contract/\(name)"))
}

/// The path of the contract's example `name`.
func example(_ name: String) -> String {
    root.appending(path: "contract/\(name)").path
}

/// Every example in `contract/`, by file name.
func examples() throws -> [String] {
    try FileManager.default.contentsOfDirectory(atPath: root.appending(path: "contract").path)
        .filter { $0.hasSuffix(".json") }.sorted()
}

/// The `result` of the contract's `hello` answer, as JSON.
func helloResult() throws -> [String: Any] {
    let hello = try JSONSerialization.jsonObject(with: contract("hello.result.json")) as? [String: Any]
    return hello?["result"] as? [String: Any] ?? [:]
}

/// The engine named in the contract's `hello`. The stand-ins treat it as
/// this build's engine.
func engine() throws -> String {
    try helloResult()["server"] as? String ?? ""
}

/// `json` serialized as JSON data.
func json(_ json: Any) throws -> Data {
    try JSONSerialization.data(withJSONObject: json)
}

/// The folder where a test run keeps the files it makes. It's removed when
/// the run ends.
let scratch: URL = {
    let folder = URL(fileURLWithPath: scratchPath())
    try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    atexit { try? FileManager.default.removeItem(atPath: scratchPath()) }
    return folder
}()

private func scratchPath() -> String {
    FileManager.default.temporaryDirectory.appending(path: "turnscope-tests-\(getpid())").path
}

/// A short socket path, since a Unix socket path can be at most 104 bytes.
func socketPath() -> String {
    scratch.appending(path: "\(UUID().uuidString.prefix(8)).sock").path
}

/// A path for a file a test watches for.
func marker() -> URL {
    scratch.appending(path: "started-\(UUID().uuidString)")
}

/// A stand-in for the `turnscope` binary. `serve --socket` prints `socket`.
/// `--version` prints `version`, which defaults to the engine named in the
/// contract's `hello`. `serve` touches `started`, which a test watches for.
func binary(socket: String, started: URL, version: String? = nil) throws -> URL {
    let folder = scratch.appending(path: "engine-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    let url = folder.appending(path: "turnscope")
    let script = """
        #!/bin/sh
        if [ "$2" = "--socket" ]; then echo '\(socket)'; exit 0; fi
        if [ "$1" = "--version" ]; then echo '\(try version ?? engine())'; exit 0; fi
        touch '\(started.path)'
        """
    try script.write(to: url, atomically: true, encoding: .utf8)
    try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
    return url
}

/// What a stand-in serve answers a request with.
enum Reply: Sendable {
    /// Its result, as JSON.
    case result(Data)
    /// An error, in the form serve sends.
    case error(code: Int = -32000, String)

    /// A result holding `json`.
    static func of(_ json: Any) -> Reply {
        .result((try? JSONSerialization.data(withJSONObject: json)) ?? Data("{}".utf8))
    }

    /// The empty result, `{}`, which most requests return.
    static let done = Reply.of([String: Any]())
}

/// A Unix socket that answers JSON-RPC as serve does. Each request gets what
/// `answer` returns for its method and parameters. `answer` is also given the
/// serve, so it can push through it.
final class FakeServe: Sendable {
    typealias Answer = @Sendable (_ method: String, _ params: [String: Any], _ serve: FakeServe) -> Reply

    let path: String
    private let listener: Int32
    private let answer: Answer
    private let state = Mutex(State())

    private struct State {
        var connections: [Int32] = []
        /// Every request and notification received, as JSON.
        var asked: [Data] = []
    }

    init(at path: String = socketPath(), answer: @escaping Answer) {
        self.path = path
        self.answer = answer
        unlink(path)
        let server = socket(AF_UNIX, SOCK_STREAM, 0)
        listener = server
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let bytes = Array(path.utf8) + [0]
        withUnsafeMutableBytes(of: &address.sun_path) { $0.copyBytes(from: bytes) }
        _ = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(server, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        listen(server, 8)
        Thread { [self] in
            while true {
                let connection = accept(server, nil, nil)
                if connection < 0 { return }
                // A client that goes away mid-write must not end the tests.
                var on: Int32 = 1
                setsockopt(connection, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
                state.withLock { $0.connections.append(connection) }
                Thread { [self] in serve(connection) }.start()
            }
        }.start()
    }

    /// Every message received so far, in order, as JSON.
    private var asked: [[String: Any]] {
        state.withLock { $0.asked }.compactMap { (try? JSONSerialization.jsonObject(with: $0)) as? [String: Any] }
    }

    /// The methods called so far, in order.
    var methods: [String] {
        asked.compactMap { $0["method"] as? String }
    }

    /// The parameters of every call to `method`.
    func params(of method: String) -> [[String: Any]] {
        asked.filter { $0["method"] as? String == method }.compactMap { $0["params"] as? [String: Any] }
    }

    /// Push the notification `method` to every connection.
    func push(_ method: String, _ params: Any) {
        send(["jsonrpc": "2.0", "method": method, "params": params], to: state.withLock { $0.connections })
    }

    /// End every connection, as happens when serve stops.
    func drop() {
        let ended = state.withLock { state in
            defer { state.connections = [] }
            return state.connections
        }
        for connection in ended {
            shutdown(connection, SHUT_RDWR)
            Darwin.close(connection)
        }
    }

    func close() {
        shutdown(listener, SHUT_RDWR)
        Darwin.close(listener)
        drop()
        unlink(path)
    }

    private func serve(_ connection: Int32) {
        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 65_536)
        while true {
            let count = read(connection, &chunk, chunk.count)
            if count <= 0 { return }
            buffer.append(contentsOf: chunk[0..<count])
            while let end = buffer.firstIndex(of: 0x0A) {
                let line = Data(buffer[buffer.startIndex..<end])
                buffer.removeSubrange(buffer.startIndex...end)
                guard let message = (try? JSONSerialization.jsonObject(with: line)) as? [String: Any],
                      let method = message["method"] as? String else { continue }
                state.withLock { $0.asked.append(line) }
                guard let id = message["id"] else { continue }
                let reply: [String: Any] = switch answer(method, message["params"] as? [String: Any] ?? [:], self) {
                case .result(let result):
                    ["jsonrpc": "2.0", "id": id, "result": (try? JSONSerialization.jsonObject(with: result)) ?? [:]]
                case .error(let code, let message):
                    ["jsonrpc": "2.0", "id": id, "error": ["code": code, "message": message]]
                }
                send(reply, to: [connection])
            }
        }
    }

    private func send(_ message: [String: Any], to connections: [Int32]) {
        guard var line = try? JSONSerialization.data(withJSONObject: message) else { return }
        line.append(0x0A)
        for connection in connections {
            line.withUnsafeBytes { _ = write(connection, $0.baseAddress, $0.count) }
        }
    }
}

/// Wait until `done` is true, for at most six seconds.
@MainActor
func until(_ done: () -> Bool) async {
    for _ in 0..<600 where !done() {
        try? await Task.sleep(for: .milliseconds(10))
    }
}
