// M0 spike: prove Quickshell's Socket + SplitParser can speak the daemon's
// NDJSON protocol with no glue. Run: quickshell -p spikes/qml-socket
import QtQuick
import Quickshell
import Quickshell.Io

ShellRoot {
  Socket {
    id: sock
    path: Quickshell.env("XDG_RUNTIME_DIR") + "/omarchy-rust-spotify/omarchy-rust-spotify.sock"
    connected: true
    property int received: 0

    parser: SplitParser {
      onRead: line => {
        const msg = JSON.parse(line)
        sock.received++
        if (msg.t === "hello") {
          console.log("hello: proto", msg.proto, "daemon", msg.daemon)
          sock.write(JSON.stringify({ t: "sub", id: 1, topics: ["player"] }) + "\n")
          sock.flush()
        } else if (msg.t === "snap") {
          const tr = msg.state.track
          console.log("snap:", msg.state.status, tr ? tr.name + " - " + tr.artists.join(", ") : "(no track)")
          // Round trip: send a command from QML and expect ok + an event.
          sock.write(JSON.stringify({ t: "cmd", id: 2, cmd: "pause" }) + "\n")
          sock.flush()
        } else if (msg.t === "ev") {
          console.log("ev:", JSON.stringify(msg.delta))
          if (msg.delta.status === "paused") {
            sock.write(JSON.stringify({ t: "cmd", id: 3, cmd: "play" }) + "\n")
            sock.flush()
          }
          if (msg.delta.status === "playing") Qt.quit()
        } else {
          console.log(msg.t, JSON.stringify(msg))
        }
      }
    }
  }
}
