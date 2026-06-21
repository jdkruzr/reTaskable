import QtQuick
import QtQuick.Controls
import net.asivery.AppLoad 1.0

Rectangle {
    id: root
    anchors.fill: parent
    color: "white"

    signal close
    function unloading() {
        endpoint.terminate()
    }

    // M9b: cursor for the currently-primed conflict. Set by parsing the
    // OPID:<n> sentinel on the first line of a type-112 response. Zero means
    // "no conflict primed, hide the K/T/C row." Cleared on type-113 outcome
    // OR when Cancel is tapped.
    property int pendingResolutionOpId: 0

    // M10: when false, the list shows only open tasks; when true, finished
    // (completed/cancelled) tasks are included. Drives the MSG 4 payload.
    property bool showCompleted: false

    // M14 UX: number of conflict-resolvable errored ops, from the MSG 104
    // envelope. The "Resolve Conflict" button is hidden unless this is > 0, so
    // it only appears when there's actually a conflict to resolve.
    property int conflictCount: 0

    // Task-detail dialog state. Opened by single-tapping a row's text area;
    // populated from that row's model entry (no backend fetch — the list
    // envelope already carries every field shown). detailDeleteArmed gates the
    // two-step delete confirm so an accidental tap can't destroy a task.
    property bool detailOpen: false
    property bool detailDeleteArmed: false
    property string detailUid: ""
    property string detailSummary: ""
    property bool detailCompleted: false
    property string detailDue: ""
    property string detailSource: ""
    property string detailMark: ""

    // M11: settings screen state.
    property bool settingsOpen: false
    property bool settingsHasPassword: false
    property string selectedCalendar: ""
    // True when the server URL/username changed since the last successful
    // calendar discovery — Save is blocked until the user re-Tests, so we never
    // persist a calendar that doesn't exist on the (new) server.
    property bool needsDiscover: false

    // M10: the task list is a structured ListModel populated from the JSON
    // envelope MSG 104 carries. Each row: { uid, summary, completed, due, mark }.
    ListModel {
        id: taskModel
    }

    // M11: calendars discovered for the Settings picker.
    ListModel {
        id: calendarsModel
    }

    AppLoad {
        id: endpoint
        applicationID: "us.reticulum.retaskable"

        onMessageReceived: (type, contents) => {
            if (type === 112) {
                // Conflict preview. First line MUST be "OPID:<n>".
                var newlineIdx = contents.indexOf("\n")
                if (newlineIdx > 0 && contents.startsWith("OPID:")) {
                    var opid = parseInt(contents.substring(5, newlineIdx), 10)
                    if (!isNaN(opid)) {
                        root.pendingResolutionOpId = opid
                        statusText.text = contents.substring(newlineIdx + 1)
                        return
                    }
                }
                statusText.text = contents
                return
            }
            if (type === 113) {
                root.pendingResolutionOpId = 0
                statusText.text = contents
                root.refreshList()
                return
            }
            if (type === 104) {
                root.applyTaskList(contents)
                return
            }
            if (type === 114) {
                root.applyToggleResult(contents)
                return
            }
            if (type === 105 || type === 108 || type === 119 || type === 120) {
                statusText.text = contents
                root.refreshList()
                return
            }
            if (type === 115) {
                root.applyConfig(contents)
                return
            }
            if (type === 116) {
                root.applySaveResult(contents)
                return
            }
            if (type === 117) {
                root.applyDiscover(contents)
                return
            }
            if (type === 118) {
                root.applyDrainResult(contents)
                return
            }
            statusText.text = contents
        }
    }

    // Ask the backend for the list in the current view mode. Open-only by
    // default; "all" includes finished tasks.
    function refreshList() {
        endpoint.sendMessage(4, root.showCompleted ? "all" : "open")
    }

    // Reply to the MSG 18 intake drain (M13). The backend has already ingested any
    // note-anchor hand-off files into the queue; load the list so freshly-captured
    // to-dos show up. Runs on app launch (the startup Timer fires the drain first).
    function applyDrainResult(jsonText) {
        var n = 0
        try {
            var d = JSON.parse(jsonText)
            n = d.ingested ? d.ingested : 0
        } catch (e) {
            // Non-JSON (e.g. "error: ..."); still load the list below.
        }
        if (n > 0) {
            console.log("retaskable: ingested " + n + " note to-do(s)")
        }
        root.refreshList()
    }

    // ---- M11: settings flow ----
    function openSettings() {
        root.settingsOpen = true
        settingsStatus.text = ""
        calendarsModel.clear()
        endpoint.sendMessage(15, "") // GET_CONFIG -> applyConfig
    }

    function applyConfig(jsonText) {
        var c
        try {
            c = JSON.parse(jsonText)
        } catch (e) {
            settingsStatus.text = jsonText
            return
        }
        settingsUrl.text = c.base_url ? c.base_url : ""
        settingsUser.text = c.username ? c.username : ""
        settingsPass.text = ""
        root.settingsHasPassword = c.has_password === true
        root.selectedCalendar = c.calendar ? c.calendar : ""
        // Setting the fields above fired onTextChanged (→ needsDiscover=true);
        // clear it, then auto-discover so the picker reflects the real server
        // and the prefilled calendar is validated against it.
        root.needsDiscover = false
        if (settingsUrl.text.trim().length > 0 && settingsUser.text.trim().length > 0) {
            root.loadCalendars()
        }
    }

    function loadCalendars() {
        settingsStatus.text = "Contacting server…"
        endpoint.sendMessage(17, JSON.stringify({
            base_url: settingsUrl.text.trim(),
            username: settingsUser.text.trim(),
            app_password: settingsPass.text
        }))
    }

    function applyDiscover(jsonText) {
        var r
        try {
            r = JSON.parse(jsonText)
        } catch (e) {
            settingsStatus.text = jsonText
            return
        }
        if (!r.ok) {
            settingsStatus.text = "Error: " + (r.error ? r.error : "could not reach server")
            return
        }
        calendarsModel.clear()
        var cals = r.calendars || []
        var stillValid = false
        for (var i = 0; i < cals.length; i++) {
            calendarsModel.append({ display_name: cals[i].display_name, href: cals[i].href })
            if (cals[i].display_name === root.selectedCalendar) {
                stillValid = true
            }
        }
        // If the previously-selected calendar isn't on this server, drop it
        // (auto-pick when there's exactly one) so we can't save a stale choice.
        if (!stillValid) {
            root.selectedCalendar = (cals.length === 1) ? cals[0].display_name : ""
        }
        root.needsDiscover = false
        settingsStatus.text = "Found " + cals.length + " calendar(s). "
            + (root.selectedCalendar ? "Selected: " + root.selectedCalendar + "."
                                     : "Pick one, then Save.")
    }

    function saveSettings() {
        settingsStatus.text = "Saving…"
        endpoint.sendMessage(16, JSON.stringify({
            base_url: settingsUrl.text.trim(),
            username: settingsUser.text.trim(),
            app_password: settingsPass.text,
            calendar: root.selectedCalendar
        }))
    }

    function applySaveResult(jsonText) {
        var r
        try {
            r = JSON.parse(jsonText)
        } catch (e) {
            settingsStatus.text = jsonText
            return
        }
        if (!r.ok) {
            settingsStatus.text = "Error: " + (r.error ? r.error : "save failed")
            return
        }
        root.settingsOpen = false
        statusText.text = "Settings saved. Syncing…"
        endpoint.sendMessage(5, "") // Sync repopulates (cache was reset if target changed)
    }

    // Jump exactly one screenful with no animation — a single, deliberate
    // e-ink refresh per tap (kinetic scrolling is disabled; see ListView).
    function pageDown() {
        var maxY = Math.max(0, taskList.contentHeight - taskList.height)
        taskList.contentY = Math.min(maxY, taskList.contentY + taskList.height)
    }
    function pageUp() {
        taskList.contentY = Math.max(0, taskList.contentY - taskList.height)
    }

    // Give the backend connection a moment to establish, then drain any note-anchor
    // intake files (MSG 18). The 118 reply (applyDrainResult) loads the task list,
    // so freshly-captured to-dos appear in a single redraw.
    Timer {
        interval: 300
        repeat: false
        running: true
        onTriggered: endpoint.sendMessage(18, "")
    }

    // Parse the MSG 104 JSON envelope and repopulate the list. Non-JSON replies
    // (e.g. "calendar ... not yet synced" or "error: ...") fall through to the
    // status line.
    function applyTaskList(jsonText) {
        var data
        try {
            data = JSON.parse(jsonText)
        } catch (e) {
            // Non-JSON reply (e.g. "calendar ... not yet synced" / "error: ...").
            // Clear the model so stale rows from a previous target don't linger,
            // and surface the message.
            taskModel.clear()
            statusText.text = jsonText
            return
        }
        taskModel.clear()
        var tasks = data.tasks || []
        for (var i = 0; i < tasks.length; i++) {
            var t = tasks[i]
            taskModel.append({
                uid: t.uid,
                summary: t.summary,
                completed: t.completed === true,
                due: t.due ? t.due : "",
                mark: t.mark ? t.mark : "",
                source: t.source ? t.source : ""
            })
        }
        root.conflictCount = data.conflicts ? data.conflicts : 0
        taskList.contentY = 0
        var synced = data.last_synced ? data.last_synced : "Not yet synced — tap Sync."
        statusText.text = synced + "   (" + tasks.length + (root.showCompleted ? " shown, incl. completed)" : " open)")
    }

    // Apply the compact MSG 114 result to a single row — no full-list redraw,
    // which would force an e-ink full refresh / ghosting.
    function applyToggleResult(jsonText) {
        var data
        try {
            data = JSON.parse(jsonText)
        } catch (e) {
            statusText.text = jsonText
            return
        }
        for (var i = 0; i < taskModel.count; i++) {
            if (taskModel.get(i).uid === data.uid) {
                taskModel.set(i, {
                    completed: data.completed === true,
                    mark: data.mark ? data.mark : ""
                })
                return
            }
        }
    }

    // Open the task-detail dialog for the row at `idx`, copying its fields out
    // of the model (the dialog reads root.detail* so it survives a list refresh).
    function openDetail(idx) {
        var t = taskModel.get(idx)
        root.detailUid = t.uid
        root.detailSummary = t.summary
        root.detailCompleted = t.completed === true
        root.detailDue = t.due ? t.due : ""
        root.detailSource = t.source ? t.source : ""
        root.detailMark = t.mark ? t.mark : ""
        root.detailDeleteArmed = false
        detailSummaryInput.text = t.summary
        root.detailOpen = true
    }

    // Close the detail dialog, releasing focus from the summary editor and
    // dismissing the virtual keyboard (the TextArea raised it on focus; nothing
    // else takes it down on the way back to the list).
    function closeDetail() {
        detailSummaryInput.focus = false
        Qt.inputMethod.hide()
        root.detailOpen = false
    }

    // Human-readable pending-sync state from the row's mark.
    function markLabel(m) {
        if (m === "!") return "Sync error — will retry"
        if (m === "*") return "Queued — not yet synced"
        return "Synced"
    }

    // ---- Header: title, status, and the essential controls ----
    Column {
        id: header
        anchors.top: parent.top
        anchors.topMargin: 40
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.leftMargin: 40
        anchors.rightMargin: 40
        spacing: 16

        Text {
            text: "reTaskable"
            font.pixelSize: 36
            color: "black"
        }

        // Fixed-height status slot: always reserves three lines (even when empty)
        // so a change in message length never reflows the buttons or list below.
        // Longer messages (errors, the conflict preview) wrap to three lines then
        // elide. The Resolve-Conflict button lives here, to the right of the
        // status, so its conditional appearance never disturbs the action-button
        // line below.
        Row {
            width: parent.width
            spacing: 16

            Text {
                id: statusText
                width: resolveConflictBtn.visible
                       ? parent.width - resolveConflictBtn.width - parent.spacing
                       : parent.width
                height: 84
                wrapMode: Text.WrapAnywhere
                maximumLineCount: 3
                elide: Text.ElideRight
                verticalAlignment: Text.AlignTop
                clip: true
                text: ""
                font.pixelSize: 20
                font.family: "monospace"
                color: "#1a1a1a"
            }

            // M14 UX: present only when the backend reports a resolvable conflict.
            Rectangle {
                id: resolveConflictBtn
                width: 300
                height: 84
                color: "white"
                border.color: "black"
                border.width: 3
                visible: root.conflictCount > 0

                Text {
                    anchors.centerIn: parent
                    text: root.conflictCount > 1
                          ? "Resolve Conflicts (" + root.conflictCount + ")"
                          : "Resolve Conflict"
                    font.pixelSize: 20
                    color: "black"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(12, "")
                }
            }
        }

        // Create a task.
        Row {
            width: parent.width
            spacing: 16

            TextField {
                id: summaryInput
                width: parent.width - createBtn.width - 16
                height: 72
                font.pixelSize: 22
                color: "black"
                placeholderTextColor: "#606060"
                placeholderText: "New task summary"
            }

            Rectangle {
                id: createBtn
                property bool active: summaryInput.text.trim().length > 0
                width: 180
                height: 72
                color: createBtn.active ? "white" : "#dddddd"
                border.color: "black"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: "Create"
                    font.pixelSize: 24
                    color: createBtn.active ? "black" : "#555555"
                }

                MouseArea {
                    anchors.fill: parent
                    enabled: createBtn.active
                    onClicked: {
                        endpoint.sendMessage(8, summaryInput.text.trim())
                        summaryInput.text = ""
                    }
                }
            }
        }

        // Action controls — all on one line: Sync, Settings, Show/Hide
        // Completed, and discrete page ▲/▼. Widths sum to fit the ~874 px
        // content width (150+160+300+90+90 + 4×16 spacing = 854). The
        // Resolve-Conflict button is not here — it lives beside the status slot
        // above so its conditional width never reflows this row.
        Row {
            width: parent.width
            spacing: 16

            Rectangle {
                width: 150
                height: 72
                color: "white"
                border.color: "black"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: "Sync"
                    font.pixelSize: 24
                    color: "black"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(5, "")
                }
            }

            Rectangle {
                width: 160
                height: 72
                color: "white"
                border.color: "black"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: "Settings"
                    font.pixelSize: 22
                    color: "black"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: root.openSettings()
                }
            }

            Rectangle {
                width: 300
                height: 72
                color: root.showCompleted ? "black" : "white"
                border.color: "black"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: root.showCompleted ? "Hide Completed" : "Show Completed"
                    font.pixelSize: 22
                    color: root.showCompleted ? "white" : "black"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: {
                        root.showCompleted = !root.showCompleted
                        root.refreshList()
                    }
                }
            }

            Rectangle {
                width: 90
                height: 72
                color: "white"
                border.color: "black"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: "▲"
                    font.pixelSize: 30
                    color: "black"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: root.pageUp()
                }
            }

            Rectangle {
                width: 90
                height: 72
                color: "white"
                border.color: "black"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: "▼"
                    font.pixelSize: 30
                    color: "black"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: root.pageDown()
                }
            }
        }

        // M9b: K/T/C row. Visible only while a conflict is primed. Cancel is
        // QML-only — no MSG round-trip, just clears the cursor so the row hides.
        Row {
            id: resolutionRow
            width: parent.width
            spacing: 16
            visible: root.pendingResolutionOpId > 0

            Rectangle {
                width: 220
                height: 72
                color: "white"
                border.color: "black"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: "Keep Mine"
                    font.pixelSize: 22
                    color: "black"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(13, "keep:" + root.pendingResolutionOpId)
                }
            }

            Rectangle {
                width: 240
                height: 72
                color: "white"
                border.color: "black"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: "Take Theirs"
                    font.pixelSize: 22
                    color: "black"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(13, "take:" + root.pendingResolutionOpId)
                }
            }

            Rectangle {
                width: 180
                height: 72
                color: "white"
                border.color: "black"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: "Cancel"
                    font.pixelSize: 22
                    color: "black"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: root.pendingResolutionOpId = 0
                }
            }
        }
    }

    // ---- The task list: the primary M10 surface ----
    ListView {
        id: taskList
        anchors.top: header.bottom
        anchors.topMargin: 20
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.leftMargin: 40
        anchors.rightMargin: 40
        anchors.bottomMargin: 40
        clip: true

        model: taskModel

        // e-ink: kinetic/flick scrolling fights the display refresh (a flick
        // spans many slow refresh cycles → lag + ghosting). Disable it entirely
        // and move the list one screenful at a time via the ▲/▼ buttons, so
        // each scroll action is a single deliberate refresh.
        interactive: false
        boundsBehavior: Flickable.StopAtBounds
        add: Transition {}
        remove: Transition {}
        displaced: Transition {}

        ScrollBar.vertical: ScrollBar {
            policy: ScrollBar.AlwaysOn
        }

        delegate: Rectangle {
            width: taskList.width
            height: 84
            color: "white"

            // Single-tap the row to open the detail dialog. Placed BELOW the Row
            // in the stack: plain Text doesn't accept taps so they fall through
            // here, while the CheckBox (above) keeps its own region. So tapping
            // the text opens detail; tapping the box still toggles.
            MouseArea {
                anchors.fill: parent
                onClicked: root.openDetail(index)
            }

            Row {
                anchors.fill: parent
                anchors.leftMargin: 8
                anchors.rightMargin: 8
                spacing: 14

                // Pending-op marker: "!" errored (red), "*" queued (amber), "" none.
                Text {
                    width: 20
                    anchors.verticalCenter: parent.verticalCenter
                    text: model.mark
                    font.pixelSize: 28
                    font.family: "monospace"
                    font.bold: true
                    color: model.mark === "!" ? "#c00000" : "#b06000"
                }

                CheckBox {
                    id: rowCheck
                    anchors.verticalCenter: parent.verticalCenter
                    checked: model.completed
                    // toggled() fires only on user interaction, not when the
                    // binding above re-evaluates after applyToggleResult().
                    onToggled: endpoint.sendMessage(14, model.uid)

                    // M14 UX: bigger tap target + a dark, clearly-bordered box
                    // with a bold check. The stock indicator is a thin light
                    // line that's hard to see and to hit on e-ink.
                    implicitWidth: 64
                    implicitHeight: 64
                    padding: 0

                    indicator: Rectangle {
                        anchors.centerIn: parent
                        width: 48
                        height: 48
                        radius: 4
                        color: "white"
                        border.color: "black"
                        border.width: 4

                        Text {
                            anchors.centerIn: parent
                            visible: rowCheck.checked
                            text: "✓"
                            font.pixelSize: 38
                            font.bold: true
                            color: "black"
                        }
                    }
                }

                Column {
                    anchors.verticalCenter: parent.verticalCenter
                    width: parent.width - 20 - rowCheck.width - 42
                    spacing: 2

                    Text {
                        width: parent.width
                        text: model.summary + (model.due ? "  (due " + model.due + ")" : "")
                        font.pixelSize: 26
                        elide: Text.ElideRight
                        color: model.completed ? "#909090" : "black"
                        font.strikeout: model.completed
                    }

                    // M13 note anchor: where this to-do was captured from. A Column
                    // skips invisible children, so unanchored rows stay single-line.
                    Text {
                        width: parent.width
                        visible: model.source && model.source.length > 0
                        text: "📓 " + model.source
                        font.pixelSize: 20
                        elide: Text.ElideRight
                        color: "#333333"
                    }
                }
            }

            Rectangle {
                anchors.bottom: parent.bottom
                width: parent.width
                height: 1
                color: "#d0d0d0"
            }
        }

        Text {
            anchors.centerIn: parent
            visible: taskModel.count === 0
            text: root.showCompleted ? "No tasks." : "No open tasks. Tap Show Completed to review done items."
            font.pixelSize: 24
            color: "#808080"
        }
    }

    // ---- M11: Settings overlay (covers the main UI while open) ----
    Rectangle {
        id: settingsOverlay
        anchors.fill: parent
        color: "white"
        visible: root.settingsOpen
        z: 100

        Column {
            anchors.top: parent.top
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.margins: 40
            spacing: 16

            Text {
                text: "Settings — Sync Target"
                font.pixelSize: 32
                font.bold: true
            }

            Text { text: "Server URL"; font.pixelSize: 18; color: "#404040" }
            TextField {
                id: settingsUrl
                width: parent.width
                height: 64
                font.pixelSize: 22
                placeholderText: "https://nextcloud.example.com"
                onTextChanged: root.needsDiscover = true
            }

            Text { text: "Username"; font.pixelSize: 18; color: "#404040" }
            TextField {
                id: settingsUser
                width: parent.width
                height: 64
                font.pixelSize: 22
                placeholderText: "username"
                onTextChanged: root.needsDiscover = true
            }

            Text { text: "App password"; font.pixelSize: 18; color: "#404040" }
            TextField {
                id: settingsPass
                width: parent.width
                height: 64
                font.pixelSize: 22
                echoMode: TextInput.Password
                placeholderText: root.settingsHasPassword
                                 ? "•••• (unchanged — leave blank to keep)"
                                 : "app password"
            }

            Rectangle {
                width: 380
                height: 64
                color: "white"
                border.color: "black"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: "Test & Load Calendars"
                    font.pixelSize: 20
                    color: "black"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: root.loadCalendars()
                }
            }

            Text {
                id: settingsStatus
                width: parent.width
                wrapMode: Text.WrapAnywhere
                text: ""
                font.pixelSize: 18
                font.family: "monospace"
                color: "#404040"
                visible: text.length > 0
            }

            Text {
                text: "Calendar" + (root.selectedCalendar ? ": " + root.selectedCalendar : " (none selected)")
                font.pixelSize: 20
                color: "black"
            }

            Column {
                width: parent.width
                spacing: 6

                Repeater {
                    model: calendarsModel

                    delegate: Rectangle {
                        width: parent.width
                        height: 60
                        color: model.display_name === root.selectedCalendar ? "black" : "white"
                        border.color: "black"
                        border.width: 2

                        Text {
                            anchors.left: parent.left
                            anchors.leftMargin: 12
                            anchors.verticalCenter: parent.verticalCenter
                            text: model.display_name
                            font.pixelSize: 22
                            color: model.display_name === root.selectedCalendar ? "white" : "black"
                        }

                        MouseArea {
                            anchors.fill: parent
                            onClicked: root.selectedCalendar = model.display_name
                        }
                    }
                }
            }

            Row {
                spacing: 16

                Rectangle {
                    id: saveBtn
                    property bool active: settingsUrl.text.trim().length > 0
                                          && settingsUser.text.trim().length > 0
                                          && root.selectedCalendar.length > 0
                                          && !root.needsDiscover
                    width: 200
                    height: 72
                    color: saveBtn.active ? "white" : "#dddddd"
                    border.color: "black"
                    border.width: 3

                    Text {
                        anchors.centerIn: parent
                        text: "Save"
                        font.pixelSize: 24
                        color: saveBtn.active ? "black" : "#888888"
                    }

                    MouseArea {
                        anchors.fill: parent
                        enabled: saveBtn.active
                        onClicked: root.saveSettings()
                    }
                }

                Rectangle {
                    width: 200
                    height: 72
                    color: "white"
                    border.color: "black"
                    border.width: 3

                    Text {
                        anchors.centerIn: parent
                        text: "Cancel"
                        font.pixelSize: 24
                        color: "black"
                    }

                    MouseArea {
                        anchors.fill: parent
                        onClicked: root.settingsOpen = false
                    }
                }
            }
        }
    }

    // ---- Task-detail dialog (opened by single-tapping a row) ----
    Rectangle {
        id: detailOverlay
        anchors.fill: parent
        color: "white"
        visible: root.detailOpen
        z: 150

        Column {
            anchors.top: parent.top
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.margins: 40
            spacing: 16

            Text {
                text: "Task"
                font.pixelSize: 32
                font.bold: true
                color: "black"
            }

            // Info block — every field comes from the tapped row (no fetch).
            Text {
                text: "Status: " + (root.detailCompleted ? "Completed" : "Open")
                font.pixelSize: 22
                color: "#1a1a1a"
            }

            Text {
                visible: root.detailDue.length > 0
                text: "Due: " + root.detailDue
                font.pixelSize: 22
                color: "#1a1a1a"
            }

            Text {
                width: parent.width
                visible: root.detailSource.length > 0
                text: "📓 " + root.detailSource
                font.pixelSize: 22
                wrapMode: Text.WrapAnywhere
                color: "#333333"
            }

            Text {
                text: root.markLabel(root.detailMark)
                font.pixelSize: 20
                color: root.detailMark === "!" ? "#c00000"
                       : (root.detailMark === "*" ? "#b06000" : "#606060")
            }

            Text {
                width: parent.width
                text: "UID: " + root.detailUid
                font.pixelSize: 15
                font.family: "monospace"
                elide: Text.ElideRight
                color: "#909090"
            }

            // Edit: a wrapping editable field shows the full summary AND edits it.
            Text {
                text: "Summary"
                font.pixelSize: 18
                color: "#404040"
            }

            TextArea {
                id: detailSummaryInput
                width: parent.width
                height: 160
                wrapMode: TextArea.Wrap
                font.pixelSize: 24
                color: "black"
                background: Rectangle {
                    border.color: "black"
                    border.width: 3
                    color: "white"
                }
            }

            Row {
                spacing: 16

                Rectangle {
                    id: detailSaveBtn
                    property bool active: detailSummaryInput.text.trim().length > 0
                                          && detailSummaryInput.text.trim() !== root.detailSummary
                    width: 240
                    height: 72
                    color: detailSaveBtn.active ? "white" : "#dddddd"
                    border.color: "black"
                    border.width: 3

                    Text {
                        anchors.centerIn: parent
                        text: "Save changes"
                        font.pixelSize: 22
                        color: detailSaveBtn.active ? "black" : "#555555"
                    }

                    MouseArea {
                        anchors.fill: parent
                        enabled: detailSaveBtn.active
                        onClicked: {
                            endpoint.sendMessage(19, JSON.stringify({
                                uid: root.detailUid,
                                summary: detailSummaryInput.text.trim()
                            }))
                            root.closeDetail()
                        }
                    }
                }

                Rectangle {
                    width: 160
                    height: 72
                    color: "white"
                    border.color: "black"
                    border.width: 3

                    Text {
                        anchors.centerIn: parent
                        text: "Close"
                        font.pixelSize: 22
                        color: "black"
                    }

                    MouseArea {
                        anchors.fill: parent
                        onClicked: root.closeDetail()
                    }
                }
            }

            // Delete: two-step confirm. A single tap arms it; a second,
            // deliberate tap on Confirm performs the delete.
            Rectangle {
                visible: !root.detailDeleteArmed
                width: 240
                height: 72
                color: "white"
                border.color: "#c00000"
                border.width: 3

                Text {
                    anchors.centerIn: parent
                    text: "Delete task"
                    font.pixelSize: 22
                    color: "#c00000"
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: root.detailDeleteArmed = true
                }
            }

            Row {
                visible: root.detailDeleteArmed
                spacing: 16

                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: "Really delete?"
                    font.pixelSize: 22
                    color: "#c00000"
                }

                Rectangle {
                    width: 200
                    height: 72
                    color: "#c00000"
                    border.color: "#c00000"
                    border.width: 3

                    Text {
                        anchors.centerIn: parent
                        text: "Confirm"
                        font.pixelSize: 22
                        color: "white"
                    }

                    MouseArea {
                        anchors.fill: parent
                        onClicked: {
                            endpoint.sendMessage(20, root.detailUid)
                            root.closeDetail()
                        }
                    }
                }

                Rectangle {
                    width: 160
                    height: 72
                    color: "white"
                    border.color: "black"
                    border.width: 3

                    Text {
                        anchors.centerIn: parent
                        text: "Keep"
                        font.pixelSize: 22
                        color: "black"
                    }

                    MouseArea {
                        anchors.fill: parent
                        onClicked: root.detailDeleteArmed = false
                    }
                }
            }
        }
    }
}
