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
            if (type === 101 || type === 102) {
                responseText.text = contents
            }
        }
    }

    Column {
        anchors.centerIn: parent
        spacing: 36

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: "reTaskable — M1"
            font.pixelSize: 40
            color: "black"
        }

        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            width: 360
            height: 100
            color: "white"
            border.color: "black"
            border.width: 3

            Text {
                anchors.centerIn: parent
                text: "Ping"
                font.pixelSize: 28
                color: "black"
            }

            MouseArea {
                anchors.fill: parent
                onClicked: endpoint.sendMessage(1, "ping")
            }
        }

        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            width: 360
            height: 100
            color: "white"
            border.color: "black"
            border.width: 3

            Text {
                anchors.centerIn: parent
                text: "Test Nextcloud"
                font.pixelSize: 28
                color: "black"
            }

            MouseArea {
                anchors.fill: parent
                onClicked: endpoint.sendMessage(2, "")
            }
        }

        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            width: 800
            height: responseText.implicitHeight + 24
            color: "white"
            border.color: "black"
            border.width: 1

            Text {
                id: responseText
                anchors.fill: parent
                anchors.margins: 12
                wrapMode: Text.Wrap
                text: "(no response yet)"
                font.pixelSize: 20
                font.family: "monospace"
                color: "black"
            }
        }
    }
}
