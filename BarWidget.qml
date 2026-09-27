import QtQuick
import QtQuick.Effects
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// The music icon for omarchy-rust-spotify. Hover it for the mini player: the
// cover (and a blur of it behind the card), the track, a live spectrum of
// what's playing, the current lyric line, seek, transport, like and volume;
// click it to open the full player; middle-click toggles playback; the
// wheel skips.
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
          root.subscribed = ""
          root.subscribe()
        } else if (msg.t === "snap") {
          root.st = msg.state
        } else if (msg.t === "ev") {
          root.st = Object.assign({}, root.st, msg.delta)
        } else if (msg.t === "viz") {
          root.bands = msg.bands || []
        } else if (msg.t === "json" && msg.id === root.lyricsReq) {
          const v = msg.value || {}
          root.lyrics = v.synced ? (v.lines || []) : []
        } else if (msg.t === "err" && msg.id === root.lyricsReq) {
          root.lyrics = []
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

  // The spectrum is only sent while the card is open on a playing track
  // (the daemon taps the audio only while someone listens).
  property string subscribed: ""
  readonly property bool wantsViz: popupOpen && isPlaying
  onWantsVizChanged: subscribe()
  function subscribe() {
    const topics = wantsViz ? ["player", "viz"] : ["player"]
    const key = topics.join(",")
    if (!sock.connected || key === subscribed) return
    subscribed = key
    sock.write(JSON.stringify({ t: "sub", id: 1, topics: topics }) + "\n")
    sock.flush()
    if (!wantsViz) bands = []
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
  readonly property string album: track ? (track.album || "") : ""
  readonly property bool liked: !!(track && track.liked)
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

  // ---- spectrum and lyrics (only while the card is open) ------------------

  // 48 log-spaced bands, 0..255, from the daemon's tap of what it plays.
  property var bands: []

  // Synced lyrics for the current track: [{ms, text}], asked for when the
  // card opens on a new track.
  property var lyrics: []
  property int lyricsReq: 0
  property string lyricsFor: ""
  function wantLyrics() {
    if (!popupOpen || !track || !sock.connected || track.uri === lyricsFor) return
    lyricsFor = track.uri
    lyrics = []
    lyricsReq = 7000 + Math.floor(Math.random() * 100000)
    sock.write(JSON.stringify({ t: "req", id: lyricsReq, req: "lyrics", uri: track.uri }) + "\n")
    sock.flush()
  }
  onTrackChanged: wantLyrics()
  readonly property string lyricLine: {
    if (!lyrics.length) return ""
    const at = positionMs + 250
    let line = ""
    for (let i = 0; i < lyrics.length && lyrics[i].ms <= at; i++) line = lyrics[i].text
    return line === "" ? "♪" : line
  }

  // ---- hover-open popup --------------------------------------------------

  property bool popupOpen: false
  function close() { popupOpen = false }

  readonly property bool hovering: iconHover.hovered || popup.containsMouse
  onHoveringChanged: {
    if (hovering) {
      closeTimer.stop()
      if (!popupOpen) openTimer.restart()
    } else if (!seek.dragging && !volume.dragging) {
      openTimer.stop()
      closeTimer.restart()
    }
  }
  // A short open delay so sweeping across the bar doesn't flash the card;
  // a longer close delay to cross the gap between bar and card.
  Timer { id: openTimer; interval: 180; onTriggered: root.popupOpen = true }
  Timer { id: closeTimer; interval: 350; onTriggered: root.popupOpen = false }

  // The clock ticks only while the card is open and something plays.
  Timer {
    interval: 250
    repeat: true
    running: root.popupOpen && root.isPlaying
    onTriggered: root.now = Date.now()
  }
  onPopupOpenChanged: {
    now = Date.now()
    wantLyrics()
  }

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

  // Colours: the bar's, dimmed in steps, and the theme accent.
  readonly property color fg: root.bar.foreground
  readonly property color dim: Qt.rgba(fg.r, fg.g, fg.b, 0.72)
  readonly property color dimmer: Qt.rgba(fg.r, fg.g, fg.b, 0.5)
  readonly property color accent: Color.accent

  // A thin rounded track with a fill and a knob; drag or click to set.
  // `value` 0..1 shows unless dragging; `commit(frac)` on release.
  component Scrub: Item {
    id: scrub
    property real value: 0
    property bool dragging: false
    property real dragValue: 0
    property bool enabledHere: true
    signal commit(real frac)
    readonly property real shown: dragging ? dragValue : Math.max(0, Math.min(1, value))
    implicitHeight: Style.space(14)

    Rectangle {
      id: groove
      anchors.verticalCenter: parent.verticalCenter
      width: parent.width
      height: Style.space(4)
      radius: height / 2
      color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.18)

      Rectangle {
        height: parent.height
        radius: parent.radius
        width: parent.width * scrub.shown
        color: area.containsMouse || scrub.dragging ? root.accent : root.fg
      }
    }
    Rectangle {
      width: Style.space(12)
      height: width
      radius: width / 2
      color: root.fg
      anchors.verticalCenter: parent.verticalCenter
      x: groove.width * scrub.shown - width / 2
      visible: scrub.enabledHere && (area.containsMouse || scrub.dragging)
    }
    MouseArea {
      id: area
      anchors.fill: parent
      anchors.margins: -Style.space(4)
      hoverEnabled: true
      enabled: scrub.enabledHere
      cursorShape: Qt.PointingHandCursor
      function frac(mx) { return Math.max(0, Math.min(1, (mx - Style.space(4)) / groove.width)) }
      onPressed: function(m) { scrub.dragging = true; scrub.dragValue = frac(m.x) }
      onPositionChanged: function(m) { if (scrub.dragging) scrub.dragValue = frac(m.x) }
      onReleased: function(m) {
        scrub.dragValue = frac(m.x)
        scrub.commit(scrub.dragValue)
        scrub.dragging = false
      }
    }
  }

  PopupCard {
    id: popup
    anchorItem: root
    bar: root.bar
    owner: root
    open: root.popupOpen
    triggerMode: "hover"
    contentWidth: popup.fittedContentWidth(Style.space(340))
    contentHeight: popup.fittedContentHeight(column.implicitHeight)

    // The cover, blurred and dimmed, behind everything: the card takes on
    // the album's colours while the theme's scrim keeps text readable.
    Item {
      id: backdrop
      z: -1
      anchors.fill: parent
      anchors.margins: -popup.padding
      visible: root.artUrl !== "" && root.problem === ""
      clip: true

      Image {
        id: backdropArt
        anchors.fill: parent
        source: backdrop.visible ? root.artUrl : ""
        fillMode: Image.PreserveAspectCrop
        asynchronous: true
        sourceSize.width: 256
        sourceSize.height: 256
        visible: false
      }
      MultiEffect {
        anchors.fill: backdropArt
        source: backdropArt
        blurEnabled: true
        blur: 1.0
        blurMax: 48
        saturation: 0.25
        opacity: backdropArt.status === Image.Ready ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: 240 } }
      }
      Rectangle {
        anchors.fill: parent
        readonly property color base: Color.popups.background
        color: Qt.rgba(base.r, base.g, base.b, 0.78)
      }
    }

    Column {
      id: column
      anchors.fill: parent
      spacing: Style.space(10)

      // Where it's playing, and a pulse while it plays.
      Row {
        width: parent.width
        spacing: Style.space(6)
        visible: root.problem === "" && root.track !== null

        Text {
          text: (root.isPlaying ? "PLAYING ON " : "PAUSED ON ") + (root.st.device_name || "OMARCHY").toUpperCase()
          color: root.dimmer
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
          font.letterSpacing: 1.2
        }
      }

      Row {
        width: parent.width
        spacing: Style.space(12)

        BorderSurface {
          width: Style.space(96)
          height: Style.space(96)
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
            color: root.fg
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.displayLarge
          }
        }

        Column {
          width: parent.width - Style.space(108)
          anchors.verticalCenter: parent.verticalCenter
          spacing: Style.space(3)

          Row {
            width: parent.width
            spacing: Style.space(4)

            Text {
              width: parent.width - (heart.visible ? heart.width + Style.space(4) : 0)
              textFormat: Text.PlainText
              text: root.problem !== "" ? root.problem : (root.title || "Nothing playing")
              color: root.fg
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.subtitle
              font.bold: true
              wrapMode: Text.WordWrap
              maximumLineCount: 2
              elide: Text.ElideRight
            }

            Button {
              id: heart
              visible: root.online && root.track !== null && root.problem === ""
              iconText: root.liked ? "󰋑" : "󰋕"
              selected: root.liked
              foreground: root.liked ? root.accent : root.fg
              horizontalPadding: Style.space(4)
              verticalPadding: Style.space(2)
              opacity: root.liked ? 1.0 : 0.6
              tooltipText: root.liked ? "Remove from Liked Songs" : "Add to Liked Songs"
              onClicked: root.send({ cmd: "like", on: !root.liked })
            }
          }

          Text {
            width: parent.width
            visible: text !== ""
            textFormat: Text.PlainText
            text: root.needsSetup ? "Installs the player, then signs you in"
              : root.problem !== "" ? (root.action ? "" : "Click to open the player")
              : root.artist
            color: root.dim
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.body
            elide: Text.ElideRight
          }

          Text {
            width: parent.width
            visible: text !== "" && root.problem === ""
            textFormat: Text.PlainText
            text: root.album
            color: root.dimmer
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
        foreground: root.fg
        bordered: true
        horizontalPadding: Style.spacing.panelGap
        verticalPadding: Style.spacing.controlPaddingY
        onClicked: {
          root.popupOpen = false
          root.action[1]()
        }
      }

      // The live spectrum of what's playing (real audio, not an animation).
      Item {
        width: parent.width
        height: Style.space(26)
        visible: root.isPlaying && root.bands.length > 0

        Row {
          anchors.bottom: parent.bottom
          anchors.horizontalCenter: parent.horizontalCenter
          spacing: Style.space(2)
          readonly property int count: 36
          readonly property real barWidth: (parent.width - spacing * (count - 1)) / count

          Repeater {
            model: parent.count
            Rectangle {
              readonly property real v: {
                const b = root.bands
                if (!b.length) return 0
                const i = Math.min(b.length - 1, Math.floor(index * b.length / 36))
                return Math.max(0, Math.min(1, (b[i] / 255 - 0.3) / 0.65))
              }
              width: parent.barWidth
              height: Math.max(Style.space(2), parent.parent.height * v)
              anchors.bottom: parent.bottom
              radius: Math.min(width / 2, Style.space(2))
              color: root.accent
              opacity: 0.35 + 0.65 * v
              Behavior on height { NumberAnimation { duration: 90 } }
            }
          }
        }
      }

      // The line being sung right now, when the song has synced lyrics.
      Text {
        width: parent.width
        visible: root.lyricLine !== "" && root.problem === ""
        textFormat: Text.PlainText
        text: root.lyricLine
        color: root.fg
        horizontalAlignment: Text.AlignHCenter
        font.family: root.bar.fontFamily
        font.pixelSize: Style.font.body
        font.italic: true
        elide: Text.ElideRight
      }

      // Seek, with elapsed and remaining.
      Column {
        width: parent.width
        spacing: Style.space(2)
        visible: root.track !== null && root.problem === ""

        Scrub {
          id: seek
          width: parent.width
          value: root.track && root.track.duration_ms > 0 ? root.positionMs / root.track.duration_ms : 0
          enabledHere: root.online
          onCommit: function(frac) {
            if (root.track) root.send({ cmd: "seek", ms: Math.round(root.track.duration_ms * frac) })
          }
        }

        Item {
          width: parent.width
          height: elapsed.implicitHeight

          Text {
            id: elapsed
            anchors.left: parent.left
            text: root.fmt(seek.dragging && root.track ? seek.dragValue * root.track.duration_ms : root.positionMs)
            color: root.dimmer
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
          }
          Text {
            anchors.right: parent.right
            text: root.track ? "-" + root.fmt(Math.max(0, root.track.duration_ms
              - (seek.dragging ? seek.dragValue * root.track.duration_ms : root.positionMs))) : ""
            color: root.dimmer
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
          }
        }
      }

      // Shuffle, previous, the big play button, next, repeat.
      Row {
        visible: root.online && root.problem === ""
        anchors.horizontalCenter: parent.horizontalCenter
        spacing: Style.space(10)

        Button {
          anchors.verticalCenter: parent.verticalCenter
          iconText: "󰒟"
          foreground: root.st.shuffle ? root.accent : root.fg
          selected: !!root.st.shuffle
          opacity: root.st.shuffle ? 1.0 : 0.55
          tooltipText: root.st.shuffle ? "Shuffle is on" : "Shuffle"
          onClicked: root.send({ cmd: "shuffle", on: !root.st.shuffle })
        }

        Button {
          anchors.verticalCenter: parent.verticalCenter
          iconText: "󰒮"
          foreground: root.fg
          iconSize: Style.font.iconLarge
          enabled: root.track !== null
          opacity: enabled ? 1.0 : 0.4
          onClicked: root.send({ cmd: "prev" })
        }

        // The play button: a filled accent circle.
        Rectangle {
          id: playButton
          anchors.verticalCenter: parent.verticalCenter
          width: Style.space(46)
          height: width
          radius: width / 2
          color: root.accent
          scale: playArea.pressed ? 0.94 : (playArea.containsMouse ? 1.05 : 1.0)
          Behavior on scale { NumberAnimation { duration: 90 } }

          Text {
            anchors.centerIn: parent
            text: root.isPlaying ? "󰏤" : "󰐊"
            color: Color.popups.background
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.iconLarge
          }
          MouseArea {
            id: playArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.send({ cmd: "play_pause" })
          }
        }

        Button {
          anchors.verticalCenter: parent.verticalCenter
          iconText: "󰒭"
          foreground: root.fg
          iconSize: Style.font.iconLarge
          enabled: root.track !== null
          opacity: enabled ? 1.0 : 0.4
          onClicked: root.send({ cmd: "next" })
        }

        Button {
          anchors.verticalCenter: parent.verticalCenter
          iconText: root.st.repeat === "track" ? "󰑘" : "󰑖"
          foreground: root.st.repeat && root.st.repeat !== "off" ? root.accent : root.fg
          selected: !!root.st.repeat && root.st.repeat !== "off"
          opacity: root.st.repeat && root.st.repeat !== "off" ? 1.0 : 0.55
          tooltipText: root.st.repeat === "track" ? "Repeating this song"
            : root.st.repeat === "context" ? "Repeating" : "Repeat"
          onClicked: {
            const next = { off: "context", context: "track", track: "off" }
            root.send({ cmd: "repeat", mode: next[root.st.repeat || "off"] })
          }
        }
      }

      // Volume (the system's, same as the bar's audio control).
      Row {
        width: parent.width
        spacing: Style.space(8)
        visible: root.online && root.problem === ""

        Text {
          anchors.verticalCenter: parent.verticalCenter
          text: (root.st.volume || 0) === 0 ? "󰝟" : (root.st.volume < 50 ? "󰖀" : "󰕾")
          color: root.dim
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.icon
        }
        Scrub {
          id: volume
          anchors.verticalCenter: parent.verticalCenter
          width: parent.width - Style.space(76)
          value: (root.st.volume || 0) / 100
          onCommit: function(frac) { root.send({ cmd: "volume", pct: Math.round(frac * 100) }) }
        }
        Text {
          anchors.verticalCenter: parent.verticalCenter
          width: Style.space(36)
          horizontalAlignment: Text.AlignRight
          text: Math.round(volume.dragging ? volume.dragValue * 100 : (root.st.volume || 0)) + "%"
          color: root.dimmer
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
        }
      }

      // Into the full player, or the visualizer window.
      Row {
        visible: root.online && root.problem === ""
        anchors.horizontalCenter: parent.horizontalCenter
        spacing: Style.space(8)

        Button {
          text: "Open player"
          iconText: "󰝚"
          foreground: root.fg
          bordered: true
          horizontalPadding: Style.spacing.controlPaddingX
          verticalPadding: Style.spacing.controlPaddingY
          onClicked: { root.popupOpen = false; root.openPlayer() }
        }
        Button {
          text: "Visualizer"
          iconText: "󰐹"
          foreground: root.fg
          bordered: true
          horizontalPadding: Style.spacing.controlPaddingX
          verticalPadding: Style.spacing.controlPaddingY
          onClicked: {
            root.popupOpen = false
            Quickshell.execDetached([root.home + "/.local/bin/omarchy-rust-spotify-viz"])
          }
        }
      }
    }
  }
}
