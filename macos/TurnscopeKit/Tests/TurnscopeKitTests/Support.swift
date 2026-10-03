// What the kit's tests share: the contract's files, and shell scripts that
// stand in for `turnscope watch`.

import Foundation

/// The contract's file `name`, which the Rust tests check too.
func contract(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appending(path: "contract/\(name)"))
}

/// The contract's file `name`, a line at a time.
func lines(_ name: String) throws -> [Data] {
    try contract(name).split(separator: 0x0A).map { Data($0) }
}

/// An executable script, run as the engine is, that `text` is the body of.
func script(_ text: String) throws -> URL {
    let folder = FileManager.default.temporaryDirectory.appending(path: "turnscope-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    let url = folder.appending(path: "turnscope")
    try "#!/bin/sh\n\(text)\n".write(to: url, atomically: true, encoding: .utf8)
    try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
    return url
}

/// A script's lines that answer each request read, by its id, with `error`,
/// a JSON value, as `turnscope watch` replies.
func replying(_ error: String) -> String {
    """
    while read -r line; do
      id=$(printf '%s' "$line" | sed 's/.*"id":\\([0-9]*\\).*/\\1/')
      printf '{"reply":{"id":%s,"error":%s}}\\n' "$id" '\(error)'
    done
    """
}

/// Wait until `done`, for five seconds at the most.
@MainActor
func until(_ done: () -> Bool) async {
    for _ in 0..<500 where !done() {
        try? await Task.sleep(for: .milliseconds(10))
    }
}
