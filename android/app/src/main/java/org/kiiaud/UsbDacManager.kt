package org.kiiaud

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbConstants
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import android.hardware.usb.UsbInterface
import android.hardware.usb.UsbManager
import android.os.Build
import android.util.Log

data class UsbDacInfo(
    val device: UsbDevice,
    val name: String,
    val vendorId: Int,
    val productId: Int,
    val isAudioStreaming: Boolean,
    var isPermissionGranted: Boolean = false,
    var isConnected: Boolean = false
)

class UsbDacManager(private val context: Context) {

    private val usbManager = context.getSystemService(Context.USB_SERVICE) as UsbManager
    private var activeConnection: UsbDeviceConnection? = null
    private var activeDevice: UsbDevice? = null

    companion object {
        private const val TAG = "UsbDacManager"
        const val ACTION_USB_PERMISSION = "org.kiiaud.USB_PERMISSION"
    }

    /**
     * Scan all connected USB devices and return list of USB Audio DACs.
     */
    fun scanDacDevices(): List<UsbDacInfo> {
        val result = mutableListOf<UsbDacInfo>()
        for (device in usbManager.deviceList.values) {
            var hasAudioStreaming = false
            for (i in 0 until device.interfaceCount) {
                val iface = device.getInterface(i)
                if (iface.interfaceClass == UsbConstants.USB_CLASS_AUDIO &&
                    iface.interfaceSubclass == 2 // AudioStreaming
                ) {
                    hasAudioStreaming = true
                    break
                }
            }

            val dacName = device.productName ?: "USB Audio Device (${String.format("%04X:%04X", device.vendorId, device.productId)})"
            result.add(
                UsbDacInfo(
                    device = device,
                    name = dacName,
                    vendorId = device.vendorId,
                    productId = device.productId,
                    isAudioStreaming = hasAudioStreaming,
                    isPermissionGranted = usbManager.hasPermission(device),
                    isConnected = (activeDevice?.deviceId == device.deviceId)
                )
            )
        }
        return result
    }

    /**
     * Request user permission to access the chosen USB DAC.
     */
    fun requestPermission(device: UsbDevice, onResult: (Boolean) -> Unit) {
        if (usbManager.hasPermission(device)) {
            onResult(true)
            return
        }

        val permissionIntent = PendingIntent.getBroadcast(
            context,
            0,
            Intent(ACTION_USB_PERMISSION),
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0
        )

        val receiver = object : BroadcastReceiver() {
            override fun onReceive(ctx: Context, intent: Intent) {
                if (intent.action == ACTION_USB_PERMISSION) {
                    val granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)
                    context.unregisterReceiver(this)
                    onResult(granted)
                }
            }
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.registerReceiver(receiver, IntentFilter(ACTION_USB_PERMISSION), Context.RECEIVER_EXPORTED)
        } else {
            context.registerReceiver(receiver, IntentFilter(ACTION_USB_PERMISSION))
        }
        usbManager.requestPermission(device, permissionIntent)
    }

    /**
     * Connect to the chosen DAC, claim interfaces, allocate ISO bandwidth,
     * and pass the file descriptor to kiiaud's Rust engine.
     */
    fun connectDac(device: UsbDevice): Boolean {
        disconnectDac()

        val connection = usbManager.openDevice(device) ?: run {
            Log.e(TAG, "Failed to open USB device: ${device.productName}")
            return false
        }

        var streamingIface: UsbInterface? = null
        var bestAltSetting = 1

        // Claim AudioControl and AudioStreaming interfaces
        for (i in 0 until device.interfaceCount) {
            val iface = device.getInterface(i)
            connection.claimInterface(iface, true)
            if (iface.interfaceClass == UsbConstants.USB_CLASS_AUDIO && iface.interfaceSubclass == 2) {
                streamingIface = iface
                // Select alt setting with highest bit resolution (usually Alt 2 or 3 for 24/32-bit)
                bestAltSetting = if (iface.alternateSettingCount > 2) 2 else 1
            }
        }

        // Bug 3 Fix: Allocate ISO bandwidth in Linux xHCI scheduler via Java API
        streamingIface?.let {
            connection.setInterface(it, bestAltSetting)
        }

        val fd = connection.fileDescriptor
        Log.i(TAG, "USB DAC ${device.productName} opened with fd: $fd -> passing to Rust")

        activeConnection = connection
        activeDevice = device

        // Pass fd into kiiaud core!
        KiiaudNative.setUsbDeviceFd(fd)
        return true
    }

    /**
     * Disconnect active USB DAC and notify Rust engine to fallback to Oboe.
     */
    fun disconnectDac() {
        if (activeConnection != null) {
            KiiaudNative.clearUsbDevice()
            try {
                activeConnection?.close()
            } catch (e: Exception) {
                Log.w(TAG, "Error closing USB connection", e)
            }
            activeConnection = null
            activeDevice = null
        }
    }

    fun getActiveDeviceName(): String? = activeDevice?.productName
}

