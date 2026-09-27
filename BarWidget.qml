import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// The music icon for omarchy-rust-spotify. Hover it for a mini player
// (cover, track, progress, shuffle / previous / play-pause / next /
// repeat); click it to open the full player; middle-click toggles
// playback; the wheel skips.
//
// Everything comes from the daemon's socket (the same NDJSON protocol as
// the TUI): state arrives the moment it changes, and controls go straight
// to the daemon, so nothing here polls or waits on MPRIS.
BarWidget {
  id: root
  moduleName: "io.github.idrewlong.omarchy-rust-spotify"

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  readonly property string home: Quickshell.env("HOME")
  readonly property string runtimeDir: Quickshell.env("XDG_RUNTIME_DIR") || "/tmp"

  // ---- daemon connection -------------------------------------------------

  property var st: ({})
  property bool online: false

  Socket {
    id: sock
    path: root.runtimeDir + "/omarchy-rust-spotify/omarchy-rust-spotify.sock"
    connected: true

    onConnectedChanged: root.online = connected

    parser: SplitParser {
      onRead: line => {
        let msg
        try { msg = JSON.parse(line) } catch (e) { return }
        if (msg.t === "hello") {
          sock.write(JSON.stringify({ t: "sub", id: 1, topics: ["player"] }) + "\n")
          sock.flush()
        } else if (msg.t === "snap") {
          root.st = msg.state
        } else if (msg.t === "ev") {
          root.st = Object.assign({}, root.st, msg.delta)
        }
      }
    }
  }

  // The daemon restarts on updates: keep trying every 2 s while offline
  // (a failed attempt doesn't change `connected`, so poke it each time).
  Timer {
    id: reconnect
    interval: 2000
    repeat: true
    running: !root.online
    onTriggered: {
      sock.connected = false
      sock.connected = true
    }
  }

  function send(cmd) {
    if (!sock.connected) return
    sock.write(JSON.stringify(Object.assign({ t: "cmd", id: 0 }, cmd)) + "\n")
    sock.flush()
  }

  function openPlayer() {
    Quickshell.execDetached([
      "omarchy-launch-or-focus-tui", "--app-id=org.omarchy.rust-spotify",
      root.home + "/.local/bin/omarchy-rust-spotify", "tui"
    ])
  }

  // ---- setup, updates, sign-in -----------------------------------------

  // This plugin's folder, from this file itself: the shell doesn't hand
  // third-party plugins their folder (Omarchy 4.0.3 and later).
  readonly property string pluginDir: decodeURIComponent(
    String(Qt.resolvedUrl(".")).replace(/^file:\/\//, "")).replace(/\/$/, "")

  // The version this plugin checkout ships, and the one installed.
  property string wantVersion: ""
  property string haveVersion: ""
  property bool haveChecked: false

  FileView {
    path: root.pluginDir + "/manifest.json"
    watchChanges: true
    printErrors: false
    onLoaded: {
      try { root.wantVersion = JSON.parse(text()).version || "" } catch (e) {}
    }
    onFileChanged: reload()
  }

  FileView {
    id: installedFile
    path: root.home + "/.local/share/omarchy-rust-spotify/installed-version"
    watchChanges: true
    printErrors: false
    onLoaded: { root.haveVersion = text().trim(); root.haveChecked = true }
    onLoadFailed: { root.haveVersion = ""; root.haveChecked = true }
    onFileChanged: reload()
  }

  // Until it's installed the file doesn't exist to watch: look again now
  // and then.
  Timer {
    interval: 3000
    repeat: true
    running: root.haveVersion === ""
    onTriggered: installedFile.reload()
  }

  readonly property bool needsSetup: haveChecked && haveVersion === "" && !online
  readonly property bool updateReady: haveVersion !== "" && wantVersion !== ""
    && haveVersion !== wantVersion

  // Setup and updates run the installer where you can watch it.
  function runInstaller() {
    Quickshell.execDetached([
      "omarchy-launch-floating-terminal-with-presentation",
      "bash '" + root.pluginDir + "/scripts/install.sh'"
    ])
  }

  // The one thing to do next, if any: [label, function].
  readonly property var action: needsSetup ? ["Set up", runInstaller]
    : updateReady ? ["Update to " + wantVersion, runInstaller]
    : !online ? ["Start the player", function() {
        Quickshell.execDetached(["systemctl", "--user", "start", "omarchy-rust-spotifyd.service"])
      }]
    : st.error === "signed_out" ? ["Sign in to Spotify", function() {
        Quickshell.execDetached([root.home + "/.local/bin/omarchy-rust-spotify", "login"])
      }]
    : null

  // ---- derived state -----------------------------------------------------

  readonly property var track: st.track || null
  readonly property bool isPlaying: st.status === "playing"
  readonly property string title: track ? track.name : ""
  readonly property string artist: track ? track.artists.join(", ") : ""
  readonly property string artUrl: track
    ? (track.cover_path ? "file://" + track.cover_path : (track.cover_url || ""))
    : ""
  readonly property string problem: needsSetup ? "Set up omarchy-rust-spotify"
    : !online ? "Player isn't running"
    : st.error === "signed_out" ? "Not signed in"
    : st.error === "premium_required" ? "Spotify Premium is required"
    : st.error === "offline" ? "Can't reach Spotify"
    : ""

  // Position isn't streamed: extrapolate from the last update while playing.
  property real now: Date.now()
  readonly property real positionMs: {
    if (!track) return 0
    let p = st.position_ms || 0
    if (isPlaying) p += Math.max(0, now - (st.position_at_unix_ms || now))
    return Math.min(p, track.duration_ms || p)
  }

  function fmt(ms) {
    const s = Math.floor(ms / 1000)
    return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0")
  }

  // ---- hover-open popup --------------------------------------------------

  property bool popupOpen: false
  function close() { popupOpen = false }

  readonly property bool hovering: iconHover.hovered || popup.containsMouse
  onHoveringChanged: {
    if (hovering) {
      closeTimer.stop()
      if (!popupOpen) openTimer.restart()
    } else {
      openTimer.stop()
      closeTimer.restart()
    }
  }
  // A short open delay so sweeping across the bar doesn't flash the card;
  // a longer close delay to cross the gap between bar and card.
  Timer { id: openTimer; interval: 180; onTriggered: root.popupOpen = true }
  Timer { id: closeTimer; interval: 350; onTriggered: root.popupOpen = false }

  // The clock only ticks while the card is open and something plays.
  Timer {
    interval: 500
    repeat: true
    running: root.popupOpen && root.isPlaying
    onTriggered: root.now = Date.now()
  }
  onPopupOpenChanged: now = Date.now()

  HoverHandler { id: iconHover }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: "󰝚"
    dimmed: !root.isPlaying
    onPressed: function(b) {
      if (b === Qt.MiddleButton) { root.send({ cmd: "play_pause" }); return }
      if (b !== Qt.LeftButton) return
      root.popupOpen = false
      if (root.needsSetup) root.runInstaller()
      else root.openPlayer()
    }
    onWheelMoved: function(delta) {
      root.send({ cmd: delta > 0 ? "prev" : "next" })
    }
  }

  PopupCard {
    id: popup
    anchorItem: root
    bar: root.bar
    owner: root
    open: root.popupOpen
    triggerMode: "hover"
    contentWidth: popup.fittedContentWidth(Style.space(300))
    contentHeight: popup.fittedContentHeight(column.implicitHeight)

    Column {
      id: column
      anchors.fill: parent
      spacing: Style.space(10)

      Row {
        width: parent.width
        spacing: Style.space(10)

        BorderSurface {
          width: Style.space(64)
          height: Style.space(64)
          radius: Style.spacing.labelGap
          color: Style.normalFillFor(root.bar.foreground, Color.accent)
          borderSpec: Border.controlSpec("normal", root.bar.foreground, Color.accent)

          Image {
            anchors.fill: parent
            anchors.margins: Style.space(2)
            fillMode: Image.PreserveAspectCrop
            asynchronous: true
            source: root.artUrl
            visible: source !== "" && status === Image.Ready
          }

          Text {
            anchors.centerIn: parent
            visible: root.artUrl === ""
            text: "󰝚"
            color: root.bar.foreground
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.displayLarge
          }
        }

        Column {
          width: parent.width - Style.space(74)
          anchors.verticalCenter: parent.verticalCenter
          spacing: Style.space(4)

          Text {
            width: parent.width
            textFormat: Text.PlainText
            text: root.problem !== "" ? root.problem : (root.title || "Nothing playing")
            color: root.bar.foreground
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.subtitle
            font.bold: true
            elide: Text.ElideRight
          }

          Text {
            width: parent.width
            visible: text !== ""
            textFormat: Text.PlainText
            text: root.needsSetup ? "Installs the player, then signs you in"
              : root.problem !== "" ? (root.action ? "" : "Click to open the player")
              : root.artist
            color: Qt.darker(root.bar.foreground, 1.3)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.bodySmall
            elide: Text.ElideRight
          }
        }
      }

      // Set up, update, start, or sign in: whatever comes next.
      Button {
        visible: root.action !== null
        anchors.horizontalCenter: parent.horizontalCenter
        text: root.action ? root.action[0] : ""
        foreground: root.bar.foreground
        bordered: true
        horizontalPadding: Style.spacing.panelGap
        verticalPadding: Style.spacing.controlPaddingY
        onClicked: {
          root.popupOpen = false
          root.action[1]()
        }
      }

      // Progress, with elapsed and remaining.
      Item {
        width: parent.width
        height: Style.space(18)
        visible: root.track !== null

        Rectangle {
          id: groove
          width: parent.width
          height: Style.space(3)
          radius: height / 2
          color: Qt.rgba(root.bar.foreground.r, root.bar.foreground.g, root.bar.foreground.b, 0.18)

          Rectangle {
            height: parent.height
            radius: parent.radius
            color: Color.accent
            width: root.track && root.track.duration_ms > 0
              ? parent.width * Math.min(1, root.positionMs / root.track.duration_ms) : 0
          }

          // Click to seek.
          MouseArea {
            anchors.fill: parent
            anchors.margins: -Style.space(6)
            cursorShape: Qt.PointingHandCursor
            onClicked: function(mouse) {
              if (!root.track) return
              const frac = Math.max(0, Math.min(1, mouse.x / groove.width))
              root.send({ cmd: "seek", ms: Math.round(root.track.duration_ms * frac) })
            }
          }
        }

        Text {
          anchors.left: parent.left
          anchors.bottom: parent.bottom
          text: root.fmt(root.positionMs)
          color: Qt.darker(root.bar.foreground, 1.5)
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
        }
        Text {
          anchors.right: parent.right
          anchors.bottom: parent.bottom
          text: root.track ? "-" + root.fmt(Math.max(0, root.track.duration_ms - root.positionMs)) : ""
          color: Qt.darker(root.bar.foreground, 1.5)
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
        }
      }

      Row {
        visible: root.online
        anchors.horizontalCenter: parent.horizontalCenter
        spacing: Style.space(4)

        Button {
          iconText: root.st.shuffle ? "󰒟" : "󰒞"
          selected: !!root.st.shuffle
          foreground: root.bar.foreground
          horizontalPadding: Style.spacing.controlPaddingX
          verticalPadding: Style.spacing.controlPaddingY
          enabled: root.online
          opacity: enabled ? (root.st.shuffle ? 1.0 : 0.55) : 0.25
          onClicked: root.send({ cmd: "shuffle", on: !root.st.shuffle })
        }

        Button {
          iconText: "󰒮"
          foreground: root.bar.foreground
          horizontalPadding: Style.spacing.controlPaddingX
          verticalPadding: Style.spacing.controlPaddingY
          enabled: root.online && root.track !== null
          opacity: enabled ? 1.0 : 0.4
          onClicked: root.send({ cmd: "prev" })
        }

        Button {
          iconText: root.isPlaying ? "󰏤" : "󰐊"
          foreground: root.bar.foreground
          horizontalPadding: Style.spacing.panelGap
          verticalPadding: Style.spacing.controlPaddingY
          iconSize: Style.font.iconLarge
          enabled: root.online
          opacity: enabled ? 1.0 : 0.4
          onClicked: root.send({ cmd: "play_pause" })
        }

        Button {
          iconText: "󰒭"
          foreground: root.bar.foreground
          horizontalPadding: Style.spacing.controlPaddingX
          verticalPadding: Style.spacing.controlPaddingY
          enabled: root.online && root.track !== null
          opacity: enabled ? 1.0 : 0.4
          onClicked: root.send({ cmd: "next" })
        }

        Button {
          iconText: root.st.repeat === "track" ? "󰑘" : "󰑖"
          selected: !!root.st.repeat && root.st.repeat !== "off"
          foreground: root.bar.foreground
          horizontalPadding: Style.spacing.controlPaddingX
          verticalPadding: Style.spacing.controlPaddingY
          enabled: root.online
          opacity: enabled ? (root.st.repeat && root.st.repeat !== "off" ? 1.0 : 0.55) : 0.25
          onClicked: {
            const next = { off: "context", context: "track", track: "off" }
            root.send({ cmd: "repeat", mode: next[root.st.repeat || "off"] })
          }
        }
      }
    }
  }
}
