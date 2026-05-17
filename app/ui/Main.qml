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
            if (type === 101 || type === 102 || type === 103 || type === 104) {
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
            text: "reTaskable — M3"
            font.pixelSize: 36
            color: "black"
        }

        Row {
            anchors.horizontalCenter: parent.horizontalCenter
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
