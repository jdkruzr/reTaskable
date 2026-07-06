import QtQuick
import QtQuick.Controls

// M16 due-date picker — a segmented, tappable/typeable date field with optional
// time, plus one-tap presets and a clear. Used in the Create row and the
// task-detail dialog. Emits its value as the normalized backend token via the
// read-only `token` property:
//   ""                -> no due
//   "YYYYMMDD"        -> all-day
//   "YYYYMMDDTHHMMSS" -> timed (floating-local; the backend writes no Z/TZID)
//
// Each segment is a bordered box wrapping a numeric TextInput; tapping focuses it
// and the on-screen keyboard types in — the same idiom as the summary fields. No
// flick/kinetic anything (e-ink): every interaction is a discrete tap.
Column {
    id: due
    spacing: 10

    // State. `hasDate` gates whether a due exists at all; `timeEnabled` reveals
    // the HH:MM segments. Digits live in the TextInputs (the source of truth);
    // `token` is computed from them.
    property bool hasDate: false
    property bool timeEnabled: false

    function pad(s, n) {
        s = "" + s
        while (s.length < n) s = "0" + s
        return s
    }

    readonly property string token: {
        if (!due.hasDate) return ""
        if (yearIn.text.length === 0 || monthIn.text.length === 0 || dayIn.text.length === 0)
            return ""
        var date = due.pad(yearIn.text, 4) + due.pad(monthIn.text, 2) + due.pad(dayIn.text, 2)
        if (!due.timeEnabled) return date
        var h = hourIn.text.length ? hourIn.text : "0"
        var mi = minIn.text.length ? minIn.text : "0"
        return date + "T" + due.pad(h, 2) + due.pad(mi, 2) + "00"
    }

    // Fill the segments from a date offset relative to today (presets).
    function presetDays(daysFromNow) {
        var dt = new Date()
        dt.setDate(dt.getDate() + daysFromNow)
        yearIn.text = "" + dt.getFullYear()
        monthIn.text = due.pad(dt.getMonth() + 1, 2)
        dayIn.text = due.pad(dt.getDate(), 2)
        due.hasDate = true
    }

    function clearDue() {
        due.hasDate = false
        due.timeEnabled = false
        yearIn.text = ""
        monthIn.text = ""
        dayIn.text = ""
        hourIn.text = ""
        minIn.text = ""
    }

    // Prefill from a cached/server DUE value (the edit dialog). Accepts
    // YYYYMMDD or YYYYMMDDTHHMMSS, tolerating a trailing Z.
    function setFromToken(raw) {
        due.clearDue()
        if (!raw || raw.length < 8) return
        var s = "" + raw
        if (s.charAt(s.length - 1) === "Z") s = s.substring(0, s.length - 1)
        yearIn.text = s.substring(0, 4)
        monthIn.text = s.substring(4, 6)
        dayIn.text = s.substring(6, 8)
        due.hasDate = true
        if (s.indexOf("T") === 8 && s.length >= 13) {
            hourIn.text = s.substring(9, 11)
            minIn.text = s.substring(11, 13)
            due.timeEnabled = true
        }
    }

    // --- a single numeric segment box -------------------------------------
    component Segment: Rectangle {
        id: segRoot
        property alias input: seg
        property int digits: 2
        property int minVal: 0
        property int maxVal: 99
        property string ph: ""
        width: digits === 4 ? 110 : 78
        height: 64
        color: "white"
        border.color: due.hasDate ? "black" : "#606060"
        border.width: 3

        Text {
            anchors.centerIn: parent
            visible: seg.text.length === 0
            text: segRoot.ph
            font.pixelSize: 28
            color: "#303030"
        }

        TextInput {
            id: seg
            anchors.fill: parent
            anchors.margins: 6
            horizontalAlignment: TextInput.AlignHCenter
            verticalAlignment: TextInput.AlignVCenter
            font.pixelSize: 28
            color: "black"
            clip: true
            inputMethodHints: Qt.ImhDigitsOnly
            maximumLength: segRoot.digits
            validator: IntValidator { bottom: segRoot.minVal; top: segRoot.maxVal }
            // Typing into any segment activates the due (so the field can be
            // filled by hand, not only via presets). Programmatic clears set ""
            // and so never trip this.
            onTextChanged: if (text.length > 0) due.hasDate = true
        }
    }

    // --- a small bordered tap button --------------------------------------
    component PillButton: Rectangle {
        property alias label: lbl.text
        property bool emphasised: false
        signal clicked
        width: lbl.implicitWidth + 36
        height: 60
        color: emphasised ? "#e8e8e8" : "white"
        border.color: "black"
        border.width: 3

        Text {
            id: lbl
            anchors.centerIn: parent
            font.pixelSize: 24
            color: "black"
        }
        MouseArea {
            anchors.fill: parent
            onClicked: parent.clicked()
        }
    }

    // Row 1: the date (+ optional time) segments and the inline controls.
    Flow {
        width: due.width
        spacing: 10

        Text {
            text: "Due:"
            font.pixelSize: 26
            color: "#1a1a1a"
            height: 64
            verticalAlignment: Text.AlignVCenter
        }

        Segment { id: yearBox; digits: 4; minVal: 2000; maxVal: 2099; ph: "YYYY" }
        Text { text: "·"; font.pixelSize: 28; height: 64; verticalAlignment: Text.AlignVCenter }
        Segment { id: monthBox; digits: 2; minVal: 1; maxVal: 12; ph: "MM" }
        Text { text: "·"; font.pixelSize: 28; height: 64; verticalAlignment: Text.AlignVCenter }
        Segment { id: dayBox; digits: 2; minVal: 1; maxVal: 31; ph: "DD" }

        // Time segments, revealed by the toggle.
        Text { visible: due.timeEnabled; text: "  "; font.pixelSize: 28; height: 64
               verticalAlignment: Text.AlignVCenter }
        Segment { id: hourBox; visible: due.timeEnabled; digits: 2; minVal: 0; maxVal: 23; ph: "HH" }
        Text { visible: due.timeEnabled; text: ":"; font.pixelSize: 28; height: 64
               verticalAlignment: Text.AlignVCenter }
        Segment { id: minBox; visible: due.timeEnabled; digits: 2; minVal: 0; maxVal: 59; ph: "MM" }

        PillButton {
            label: due.timeEnabled ? "- Time" : "+ Time"
            onClicked: {
                due.timeEnabled = !due.timeEnabled
                if (due.timeEnabled) {
                    if (!due.hasDate) due.presetDays(0)
                    if (hourIn.text.length === 0) hourIn.text = "09"
                    if (minIn.text.length === 0) minIn.text = "00"
                }
            }
        }
        PillButton { label: "Clear"; onClicked: due.clearDue() }
    }

    // Row 2: quick presets.
    Flow {
        width: due.width
        spacing: 10
        PillButton { label: "Today"; onClicked: due.presetDays(0) }
        PillButton { label: "Tomorrow"; onClicked: due.presetDays(1) }
        PillButton { label: "+1 wk"; onClicked: due.presetDays(7) }
    }

    // Expose the inner TextInputs by stable id for token/preset logic.
    property TextInput yearIn: yearBox.input
    property TextInput monthIn: monthBox.input
    property TextInput dayIn: dayBox.input
    property TextInput hourIn: hourBox.input
    property TextInput minIn: minBox.input
}
