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
            if (type === 101) {
                responseText.text = contents
            }
        }
    }

    Column {
        anchors.centerIn: parent
        spacing: 48

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: "reTaskable — M0"
            font.pixelSize: 40
            color: "black"
        }

        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            width: 360
            height: 120
            color: "white"
            border.color: "black"
            border.width: 3

            Text {
                anchors.centerIn: parent
                text: "Ping"
                font.pixelSize: 32
                color: "black"
            }

            MouseArea {
                anchors.fill: parent
                onClicked: endpoint.sendMessage(1, "ping")
            }
        }

        Text {
            id: responseText
            anchors.horizontalCenter: parent.horizontalCenter
            text: "(no response yet)"
            font.pixelSize: 28
            color: "black"
        }
    }
}
