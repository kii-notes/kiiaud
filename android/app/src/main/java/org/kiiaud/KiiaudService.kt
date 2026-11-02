package org.kiiaud

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log

class KiiaudService : Service() {

    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private lateinit var usbDacManager: UsbDacManager

    companion object {
        private const val TAG = "KiiaudService"
        const val CHANNEL_ID = "kiiaud_playback_channel"
        const val NOTIFICATION_ID = 1001

        @Volatile
        var isServiceRunning = false
            private set
    }

    private val usbReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            when (intent.action) {
                UsbManager.ACTION_USB_DEVICE_ATTACHED -> {
                    val device: UsbDevice? = intent.getParcelableExtra(UsbManager.EXTRA_DEVICE)
                    Log.i(TAG, "USB Device Attached: ${device?.productName}")
                    device?.let {
                        usbDacManager.requestPermission(it) { granted ->
                            if (granted) {
                                usbDacManager.connectDac(it)
                                updateNotification("Connected: ${it.productName} (Bit-Perfect)")
                            }
                        }
                    }
                }
                UsbManager.ACTION_USB_DEVICE_DETACHED -> {
                    val device: UsbDevice? = intent.getParcelableExtra(UsbManager.EXTRA_DEVICE)
                    Log.i(TAG, "USB Device Detached: ${device?.productName}")
                    usbDacManager.disconnectDac()
                    updateNotification("DAC Detached (Fallback to Oboe)")
                }
            }
        }
    }

    override fun onCreate() {
        super.onCreate()
        isServiceRunning = true
        usbDacManager = UsbDacManager(this)

        // 1. Acquire Partial WakeLock & WifiLock (High Performance)
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "kiiaud:playback").apply { acquire() }

        val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        wifiLock = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "kiiaud:stream").apply { acquire() }

        // 2. Register USB Hot-plug Receiver
        val filter = IntentFilter().apply {
            addAction(UsbManager.ACTION_USB_DEVICE_ATTACHED)
            addAction(UsbManager.ACTION_USB_DEVICE_DETACHED)
        }
        registerReceiver(usbReceiver, filter)

        // 3. Create Notification Channel
        createNotificationChannel()

        // 4. Start as Foreground Service
        val notification = buildNotification("kiiaud engine running on port 7110")
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }

        Log.i(TAG, "Kiiaud Foreground Service initialized")
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "kiiaud Playback Service",
                NotificationManager.IMPORTANCE_LOW
            ).apply {
                description = "Foreground playback notification for kiiaud"
                setShowBadge(false)
            }
            val manager = getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(channel)
        }
    }

    private fun buildNotification(statusText: String): Notification {
        val tapIntent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        val pendingIntent = PendingIntent.getActivity(
            this,
            0,
            tapIntent,
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) PendingIntent.FLAG_IMMUTABLE else 0
        )

        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
        } else {
            Notification.Builder(this)
        }

        return builder
            .setContentTitle("kiiaud Hi-Fi Renderer")
            .setContentText(statusText)
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setContentIntent(pendingIntent)
            .setOngoing(true)
            .build()
    }

    fun updateNotification(statusText: String) {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.notify(NOTIFICATION_ID, buildNotification(statusText))
    }

    override fun onDestroy() {
        isServiceRunning = false
        try {
            unregisterReceiver(usbReceiver)
        } catch (_: Exception) {}

        usbDacManager.disconnectDac()
        wakeLock?.release()
        wifiLock?.release()
        super.onDestroy()
        Log.i(TAG, "Kiiaud Foreground Service destroyed")
    }

    override fun onBind(intent: Intent?): IBinder? = null
}

