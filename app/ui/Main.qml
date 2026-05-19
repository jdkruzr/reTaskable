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
    // OPID:<n> sentinel on the first line of a type-112 response. Zero
    // means "no conflict primed, hide the K/T/C row." Cleared on
    // type-113 outcome OR when Cancel is tapped.
    property int pendingResolutionOpId: 0

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
                        responseText.text = contents.substring(newlineIdx + 1)
                        return
                    }
                }
                // Malformed sentinel: render verbatim, leave row hidden.
                responseText.text = contents
                return
            }
            if (type === 113) {
                // Decision outcome. Clear the cursor; the row hides itself.
                root.pendingResolutionOpId = 0
                responseText.text = contents
                return
            }
            if (type === 101 || type === 102 || type === 103 || type === 104 || type === 105 || type === 106 || type === 107 || type === 108 || type === 109 || type === 110 || type === 111) {
                responseText.text = contents
            }
        }
    }

    Column {
        anchors.top: parent.top
        anchors.topMargin: 40
        anchors.horizontalCenter: parent.horizontalCenter
        width: parent.width - 80
        spacing: 24

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: "reTaskable — M9b"
            font.pixelSize: 36
            color: "black"
        }

        Row {
            width: parent.width
            spacing: 16

            TextField {
                id: summaryInput
                width: parent.width - createBtn.width - editBtn.width - 32
                height: 80
                font.pixelSize: 22
                placeholderText: "Task summary"
            }
            Rectangle {
                id: createBtn
                property bool enabled: summaryInput.text.trim().length > 0
                width: 200; height: 80
                color: createBtn.enabled ? "white" : "#dddddd"
                border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: "Create"
                    font.pixelSize: 24
                    color: createBtn.enabled ? "black" : "#888888"
                }
                MouseArea {
                    anchors.fill: parent
                    enabled: createBtn.enabled
                    onClicked: {
                        endpoint.sendMessage(8, summaryInput.text.trim())
                        summaryInput.text = ""
                    }
                }
            }
            Rectangle {
                id: editBtn
                property bool enabled: summaryInput.text.trim().length > 0
                width: 240; height: 80
                color: editBtn.enabled ? "white" : "#dddddd"
                border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: "Edit First"
                    font.pixelSize: 22
                    color: editBtn.enabled ? "black" : "#888888"
                }
                MouseArea {
                    anchors.fill: parent
                    enabled: editBtn.enabled
                    onClicked: {
                        endpoint.sendMessage(9, summaryInput.text.trim())
                        summaryInput.text = ""
                    }
                }
            }
        }

        Flow {
            width: parent.width
            spacing: 16

            Rectangle {
                width: 200; height: 80
                color: "white"; border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: "Ping"
                    font.pixelSize: 24
                    color: "black"
                }
                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(1, "ping")
                }
            }
            Rectangle {
                width: 240; height: 80
                color: "white"; border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: "Test Nextcloud"
                    font.pixelSize: 22
                    color: "black"
                }
                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(2, "")
                }
            }
            Rectangle {
                width: 240; height: 80
                color: "white"; border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: "List Calendars"
                    font.pixelSize: 22
                    color: "black"
                }
                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(3, "")
                }
            }
            Rectangle {
                width: 200; height: 80
                color: "white"; border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: "Show Tasks"
                    font.pixelSize: 22
                    color: "black"
                }
                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(4, "")
                }
            }
            Rectangle {
                width: 160; height: 80
                color: "white"; border.color: "black"; border.width: 3
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
                width: 240; height: 80
                color: "white"; border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: "Toggle First"
                    font.pixelSize: 22
                    color: "black"
                }
                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(6, "")
                }
            }
            Rectangle {
                id: deleteBtn
                property bool armed: false
                width: 280; height: 80
                color: deleteBtn.armed ? "black" : "white"
                border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: deleteBtn.armed ? "Tap again to confirm" : "Delete First"
                    font.pixelSize: 20
                    color: deleteBtn.armed ? "white" : "black"
                }
                Timer {
                    id: deleteArmTimer
                    interval: 3000
                    onTriggered: deleteBtn.armed = false
                }
                MouseArea {
                    anchors.fill: parent
                    onClicked: {
                        if (deleteBtn.armed) {
                            deleteBtn.armed = false
                            deleteArmTimer.stop()
                            endpoint.sendMessage(7, "")
                        } else {
                            deleteBtn.armed = true
                            deleteArmTimer.restart()
                        }
                    }
                }
            }
            Rectangle {
                width: 240; height: 80
                color: "white"; border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: "Show Pending"
                    font.pixelSize: 22
                    color: "black"
                }
                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(10, "")
                }
            }
            Rectangle {
                id: clearErroredBtn
                property bool armed: false
                width: 260; height: 80
                color: clearErroredBtn.armed ? "black" : "white"
                border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: clearErroredBtn.armed ? "Tap again to confirm" : "Clear Errored"
                    font.pixelSize: 20
                    color: clearErroredBtn.armed ? "white" : "black"
                }
                Timer {
                    id: clearErroredArmTimer
                    interval: 3000
                    onTriggered: clearErroredBtn.armed = false
                }
                MouseArea {
                    anchors.fill: parent
                    onClicked: {
                        if (clearErroredBtn.armed) {
                            clearErroredBtn.armed = false
                            clearErroredArmTimer.stop()
                            endpoint.sendMessage(11, "")
                        } else {
                            clearErroredBtn.armed = true
                            clearErroredArmTimer.restart()
                        }
                    }
                }
            }
            Rectangle {
                width: 300; height: 80
                color: "white"; border.color: "black"; border.width: 3
                Text {
                    anchors.centerIn: parent
                    text: "Resolve First Conflict"
                    font.pixelSize: 20
                    color: "black"
                }
                MouseArea {
                    anchors.fill: parent
                    onClicked: endpoint.sendMessage(12, "")
                }
            }
        }

        // M9b: K/T/C row. Visible only while a conflict is primed
        // (root.pendingResolutionOpId > 0). Cancel is QML-only — no MSG
        // round-trip, just clears the cursor so the row hides.
        Row {
            id: resolutionRow
            width: parent.width
            spacing: 16
            visible: root.pendingResolutionOpId > 0

            Rectangle {
                width: 220; height: 80
                color: "white"; border.color: "black"; border.width: 3
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
                width: 240; height: 80
                color: "white"; border.color: "black"; border.width: 3
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
                width: 180; height: 80
                color: "white"; border.color: "black"; border.width: 3
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

        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            width: parent.width
            height: responseText.implicitHeight + 24
            color: "white"
            border.color: "black"
            border.width: 1

            Text {
                id: responseText
                anchors.fill: parent
                anchors.margins: 12
                wrapMode: Text.WrapAnywhere
                text: "(no response yet)"
                font.pixelSize: 18
                font.family: "monospace"
                color: "black"
            }
        }
    }
}
