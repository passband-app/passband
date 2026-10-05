// A WAY TO SEE THE PHONE WITHOUT A DAEMON OR A THUMB. Debug builds only.
//
// The simulator has no tap tool and a fresh one has no account, so a change to
// this shell could be compiled and never looked at. Launched with
// `--fixture-inbox` the app boots straight onto the practice inbox's fixture
// mail (the same `RehearsalAPI` the Mac's lesson uses), and two more arguments
// put a screen up for a screenshot:
//
//   xcrun simctl launch <sim> app.passband.client.ios --fixture-inbox \
//       --fixture-tab mail --fixture-scene thread
//
// Nothing here ships: the file is fenced, and so is its one call site.

#if DEBUG
    import Foundation

    @MainActor
    enum FixtureStage {
        private static let args = ProcessInfo.processInfo.arguments

        static let on = args.contains("--fixture-inbox")

        private static func value(_ flag: String) -> String? {
            guard let i = args.firstIndex(of: flag) else { return nil }
            return args[safe: i + 1]
        }

        /// `sitrep` (the default), `records`, `mail` or `search`.
        static var tab: String? { value("--fixture-tab") }
        /// What to raise once the board has loaded; see `stage`.
        static var scene: String? { value("--fixture-scene") }

        /// Boot onto the fixture mailbox, then raise the scene asked for.
        static func boot(_ store: AppStore) async {
            await store.enterPractice()
            await store.warmPractice()
            guard let scene else { return }
            // The shell mounts on the connect; its stacks have to exist before
            // anything is pushed onto one.
            try? await Task.sleep(for: .milliseconds(900))
            stage(scene, store)
        }

        private static func stage(_ scene: String, _ store: AppStore) {
            let first = store.sitrep.standing.first
            switch scene {
            case "thread":
                if let first { store.openThread(first.thread_id, queue: store.sitrep.standing, from: .sitrep) }
            case "remind":
                if let first {
                    store.openRemind(
                        RemindTarget(
                            messageId: first.id, sender: first.sender, subject: first.one_line,
                            remindAt: first.remind_at))
                }
            case "compose":
                store.openComposeNew()
            case "tune":
                if let first { Actions.tune(sender: first.sender) }
            default:
                break
            }
        }
    }
#endif
