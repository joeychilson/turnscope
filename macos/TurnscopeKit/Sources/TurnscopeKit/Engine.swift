// The app's line to the engine: `turnscope watch`, run as a process of its
// own, which writes what the app shows as JSON lines and takes its requests
// on standard input (`crates/cli/src/watch.rs`). It stops when its input
// closes, so it never outlives the app.
//
// A process that ends while the app runs is started again, after a wait that
// doubles each time, from a second to a minute, and starts over once one has
// run a minute: a failing engine is asked again, never in a loop.
//
// What a process writes reaches the main actor in the order it was written,
// on the main queue, and only while that process is the one running: each
// run has its own number, and what a run that ended wrote late is dropped
// rather than joined to the next one's lines. A request written to a process
// that stopped reading fails, rather than raising SIGPIPE, which would end
// the app.

import Foundation
import os

/// Runs `turnscope watch` and relays what it says.
@MainActor
public final class Engine {
    /// Whether the process runs.
    public enum Phase: Equatable, Sendable {
        case starting
        case running
        /// It ended, saying `why` last, and waits to start again.
        case failed(why: String)
    }

    public private(set) var phase: Phase = .starting {
        didSet { onPhase(phase) }
    }

    /// Called with each line it writes but replies, which go to their
    /// requests.
    public var onMessage: (Message) -> Void = { _ in }
    /// Called as its phase changes.
    public var onPhase: (Phase) -> Void = { _ in }

    private let binary: URL
    private let arguments: [String]
    private var process: Process?
    private var input: FileHandle?
    /// Which run the process is, counting from 1.
    private var run = 0
    private var buffer = Data()
    private var lastSaid = ""
    private var nextID = 1
    private var waiting: [Int: (String?) -> Void] = [:]
    private var wait: TimeInterval = 1
    private var started = Date.distantPast
    private var stopping = false
    private let log = Logger(subsystem: "com.joeychilson.turnscope", category: "engine")

    /// An engine run from `binary`, given `arguments` after `watch`, as
    /// `--data` for a scratch directory.
    public init(binary: URL, arguments: [String] = []) {
        self.binary = binary
        self.arguments = arguments
    }

    /// Start it.
    public func start() {
        stopping = false
        run += 1
        let run = run
        buffer.removeAll()
        lastSaid = ""
        let process = Process()
        process.executableURL = binary
        process.arguments = ["watch"] + arguments
        let (stdin, stdout, stderr) = (Pipe(), Pipe(), Pipe())
        process.standardInput = stdin
        process.standardOutput = stdout
        process.standardError = stderr
        stdout.fileHandleForReading.readabilityHandler = Engine.relay { [weak self] data in
            self?.read(data, run: run)
        }
        stderr.fileHandleForReading.readabilityHandler = Engine.relay { [weak self] data in
            self?.said(data, run: run)
        }
        process.terminationHandler = { [weak self] process in
            let status = process.terminationStatus
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.ended(status: status, run: run) }
            }
        }
        do {
            try process.run()
        } catch {
            stdout.fileHandleForReading.readabilityHandler = nil
            stderr.fileHandleForReading.readabilityHandler = nil
            fail("Turnscope couldn't start its engine: \(error.localizedDescription)")
            return
        }
        let writing = stdin.fileHandleForWriting
        _ = fcntl(writing.fileDescriptor, F_SETNOSIGPIPE, 1)
        self.process = process
        input = writing
        started = .now
        phase = .running
    }

    /// Stop it, for good.
    public func stop() {
        stopping = true
        try? input?.close()
        process?.terminate()
    }

    /// Ask `request` of it, and call `done` with its reply: nil once done, or
    /// why it couldn't be.
    public func send(_ request: Request, done: @escaping (String?) -> Void = { _ in }) {
        guard let input, phase == .running else {
            done("Turnscope's engine isn't running")
            return
        }
        let id = nextID
        nextID += 1
        waiting[id] = done
        do {
            try input.write(contentsOf: request.line(id: id))
        } catch {
            waiting.removeValue(forKey: id)?("Turnscope's engine isn't running")
        }
    }

    /// A pipe's handler that hands what it reads to `take` on the main
    /// queue, in order, and is done at the pipe's end.
    private nonisolated static func relay(
        _ take: @escaping @MainActor (Data) -> Void
    ) -> @Sendable (FileHandle) -> Void {
        { handle in
            let data = handle.availableData
            if data.isEmpty {
                handle.readabilityHandler = nil
                return
            }
            DispatchQueue.main.async { MainActor.assumeIsolated { take(data) } }
        }
    }

    private func read(_ data: Data, run: Int) {
        guard run == self.run else { return }
        buffer.append(data)
        while let end = buffer.firstIndex(of: 0x0A) {
            let line = buffer[buffer.startIndex..<end]
            buffer.removeSubrange(buffer.startIndex...end)
            guard !line.isEmpty else { continue }
            do {
                switch try Message.decode(Data(line)) {
                case .reply(let id, let error):
                    waiting.removeValue(forKey: id)?(error)
                case let message:
                    onMessage(message)
                }
            } catch {
                log.error("a line it can't read: \(error, privacy: .public)")
            }
        }
    }

    private func said(_ data: Data, run: Int) {
        guard run == self.run, let text = String(data: data, encoding: .utf8) else { return }
        for line in text.split(separator: "\n") where !line.isEmpty {
            log.notice("\(line, privacy: .public)")
            lastSaid = String(line)
        }
    }

    private func ended(status: Int32, run: Int) {
        guard run == self.run else { return }
        process = nil
        input = nil
        for done in waiting.values { done("Turnscope's engine stopped") }
        waiting.removeAll()
        guard !stopping else { return }
        let why = lastSaid.isEmpty ? "Turnscope's engine stopped (\(status))" : lastSaid
        fail(why.hasPrefix("turnscope: ") ? String(why.dropFirst(11)) : why)
    }

    private func fail(_ why: String) {
        phase = .failed(why: why)
        // A run of a minute or more was no failure to start: begin again.
        if Date.now.timeIntervalSince(started) > 60 { wait = 1 }
        let after = wait
        wait = min(wait * 2, 60)
        Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(after))
            guard let self, !self.stopping, self.process == nil else { return }
            self.start()
        }
    }
}
