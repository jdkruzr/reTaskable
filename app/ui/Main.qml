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

    AppLoad {
        id: endpoint
        applicationID: "us.reticulum.retaskable"

        onMessageReceived: (type, contents) => {
            if (type === 101 || type === 102 || type === 103 || type === 104 || type === 105 || type === 106 || type === 107 || type === 108 || type === 109) {
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
            text: "reTaskable — M8"
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
