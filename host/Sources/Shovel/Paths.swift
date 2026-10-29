import Foundation

enum Paths {
    static var root: URL {
        if let custom = ProcessInfo.processInfo.environment["SHOVEL_HOME"], !custom.isEmpty {
            return URL(fileURLWithPath: custom)
        }
        return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".shovel")
    }

    static var socket: URL { root.appendingPathComponent("host.sock") }
    static var ipswCache: URL { root.appendingPathComponent("ipsw") }
}

struct ShovelError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

func describe(_ error: Error) -> String {
    if let error = error as? ShovelError { return error.description }
    return (error as NSError).localizedDescription
}
