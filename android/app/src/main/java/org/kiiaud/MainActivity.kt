package org.kiiaud

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Bundle
import android.os.Environment
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import androidx.core.content.ContextCompat
import java.io.File
import java.net.NetworkInterface
import java.util.Collections

class MainActivity : Activity() {

    private lateinit var usbDacManager: UsbDacManager
    private lateinit var tvStatus: TextView
    private lateinit var tvIpAddress: TextView
    private lateinit var btnToggleService: Button
    private lateinit var btnScanDacs: Button
    private lateinit var btnOpenWebUi: Button
    private lateinit var btnBackendMode: Button
    private lateinit var btnImportConfig: Button
    private lateinit var layoutDacList: LinearLayout

    private val backendModes = listOf("auto", "android", "android-usb")

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        usbDacManager = UsbDacManager(this)

        tvStatus = findViewById(R.id.tvStatus)
        tvIpAddress = findViewById(R.id.tvIpAddress)
        btnToggleService = findViewById(R.id.btnToggleService)
        btnScanDacs = findViewById(R.id.btnScanDacs)
        btnOpenWebUi = findViewById(R.id.btnOpenWebUi)
        btnBackendMode = findViewById(R.id.btnBackendMode)
        btnImportConfig = findViewById(R.id.btnImportConfig)
        layoutDacList = findViewById(R.id.layoutDacList)

        btnToggleService.setOnClickListener {
            toggleService()
        }

        btnScanDacs.setOnClickListener {
            refreshDacList()
        }

        btnOpenWebUi.setOnClickListener {
            val ip = getDeviceIpAddress()
            val intent = Intent(Intent.ACTION_VIEW, Uri.parse("http://$ip:7110"))
            startActivity(intent)
        }

        btnBackendMode.setOnClickListener {
            cycleBackendMode()
        }

        btnImportConfig.setOnClickListener {
            importConfigFromDownloads()
        }

        // Auto-start Service if not running
        if (!KiiaudService.isServiceRunning) {
            startKiiaudService()
        }

        updateUiState()
        refreshDacList()
    }

    override fun onResume() {
        super.onResume()
        updateUiState()
        refreshDacList()
    }

    private fun startKiiaudService() {
        val intent = Intent(this, KiiaudService::class.java)
        ContextCompat.startForegroundService(this, intent)
        updateUiState()
    }

    private fun stopKiiaudService() {
        val intent = Intent(this, KiiaudService::class.java)
        stopService(intent)
        updateUiState()
    }

    private fun toggleService() {
        if (KiiaudService.isServiceRunning) {
            stopKiiaudService()
        } else {
            startKiiaudService()
        }
    }

    private fun cycleBackendMode() {
        val current = KiiaudService.getSelectedBackend(this)
        val idx = backendModes.indexOf(current).let { if (it < 0) 0 else it }
        val next = backendModes[(idx + 1) % backendModes.size]
        KiiaudService.setSelectedBackend(this, next)
        if (KiiaudService.isServiceRunning) {
            stopKiiaudService()
            startKiiaudService()
        }
        updateUiState()
        Toast.makeText(this, "Audio backend set to: $next", Toast.LENGTH_SHORT).show()
    }

    private fun importConfigFromDownloads() {
        val downloads = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)
        val candidates = listOf(
            File(downloads, "kiiaud.toml"),
            File(downloads, "config.toml"),
            File("/sdcard/Download/kiiaud.toml"),
            File("/data/local/tmp/kiiaud/config.toml")
        )
        val source = candidates.firstOrNull { it.isFile && it.canRead() }
        if (source == null) {
            Toast.makeText(
                this,
                "Place kiiaud.toml in /sdcard/Download/ and tap IMPORT CONFIG again",
                Toast.LENGTH_LONG
            ).show()
            return
        }
        try {
            val dest = KiiaudService.getConfigFile(this)
            source.copyTo(dest, overwrite = true)
            if (KiiaudService.isServiceRunning) {
                stopKiiaudService()
                startKiiaudService()
            }
            Toast.makeText(this, "Imported ${source.name} -> restarted engine", Toast.LENGTH_SHORT).show()
        } catch (e: Exception) {
            Toast.makeText(this, "Import failed: ${e.message}", Toast.LENGTH_LONG).show()
        }
    }

    private fun updateUiState() {
        val isRunning = KiiaudService.isServiceRunning
        tvStatus.text = if (isRunning) "● RUNNING (Port 7110)" else "○ STOPPED"
        btnToggleService.text = if (isRunning) "STOP SERVICE" else "START SERVICE"

        val backend = KiiaudService.getSelectedBackend(this)
        btnBackendMode.text = when (backend) {
            "android" -> "BACKEND: AAUDIO"
            "android-usb" -> "BACKEND: DIRECT USB"
            else -> "BACKEND: AUTO (USB/AAUDIO)"
        }

        val ip = getDeviceIpAddress()
        tvIpAddress.text = "http://$ip:7110"
    }

    private fun refreshDacList() {
        layoutDacList.removeAllViews()
        val dacs = usbDacManager.scanDacDevices()

        if (dacs.isEmpty()) {
            val emptyTv = TextView(this).apply {
                text = "No USB Audio DAC detected.\nConnect your DAC via USB-C OTG cable."
                setPadding(16, 24, 16, 24)
                setTextColor(0xFF888888.toInt())
            }
            layoutDacList.addView(emptyTv)
            return
        }

        for (dac in dacs) {
            val dacRow = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                setPadding(24, 20, 24, 20)
                setBackgroundColor(if (dac.isConnected) 0xFF1E293B.toInt() else 0xFF0F172A.toInt())
            }

            val titleTv = TextView(this).apply {
                text = if (dac.isConnected) "◉ ${dac.name} (Active)" else "○ ${dac.name}"
                textSize = 16f
                setTextColor(if (dac.isConnected) 0xFF38BDF8.toInt() else 0xFFF8FAFC.toInt())
            }
            dacRow.addView(titleTv)

            val detailsTv = TextView(this).apply {
                text = "VID: 0x${String.format("%04X", dac.vendorId)} | PID: 0x${String.format("%04X", dac.productId)} | Bit-Perfect UAC2"
                textSize = 12f
                setTextColor(0xFF94A3B8.toInt())
                setPadding(0, 4, 0, 8)
            }
            dacRow.addView(detailsTv)

            val selectBtn = Button(this).apply {
                text = if (dac.isConnected) "ACTIVE DAC" else "SELECT THIS DAC"
                isEnabled = !dac.isConnected
                isFocusable = true
                setBackgroundResource(R.drawable.tv_button_bg)
                setTextColor(0xFFF8FAFC.toInt())
                setOnClickListener {
                    usbDacManager.requestPermission(dac.device) { granted ->
                        if (granted) {
                            val success = usbDacManager.connectDac(dac.device)
                            if (success) {
                                Toast.makeText(this@MainActivity, "Connected to ${dac.name}", Toast.LENGTH_SHORT).show()
                                refreshDacList()
                            }
                        } else {
                            Toast.makeText(this@MainActivity, "USB permission denied", Toast.LENGTH_SHORT).show()
                        }
                    }
                }
            }
            dacRow.addView(selectBtn)
            layoutDacList.addView(dacRow)
        }
    }

    private fun getDeviceIpAddress(): String {
        try {
            val interfaces = Collections.list(NetworkInterface.getNetworkInterfaces())
            for (intf in interfaces) {
                val addrs = Collections.list(intf.inetAddresses)
                for (addr in addrs) {
                    if (!addr.isLoopbackAddress) {
                        val sAddr = addr.hostAddress ?: continue
                        val isIPv4 = sAddr.indexOf(':') < 0
                        if (isIPv4) return sAddr
                    }
                }
            }
        } catch (_: Exception) {}
        return "127.0.0.1"
    }
}

