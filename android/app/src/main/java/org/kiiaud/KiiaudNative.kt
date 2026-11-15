package org.kiiaud

/**
 * JNI Bridge to kiiaud's Pure Rust Engine (libkiiaud_android.so).
 */
object KiiaudNative {

    init {
        try {
            System.loadLibrary("kiiaud_android")
        } catch (e: UnsatisfiedLinkError) {
            e.printStackTrace()
        }
    }

    /**
     * Start the kiiaud playback daemon in-process on a background Tokio runtime.
     * Returns 0 on success.
     */
    external fun startDaemon(configPath: String, backendOverride: String): Int

    /**
     * Gracefully stop the kiiaud daemon and wait for shutdown.
     */
    external fun stopDaemon()

    /**
     * Check whether the in-process kiiaud daemon is currently running.
     */
    external fun isDaemonRunning(): Boolean

    /**
     * Pass the active USB DAC file descriptor into Rust core.
     * Triggers dynamic switch to Direct USB UAC2 Bit-Perfect Backend.
     */
    external fun setUsbDeviceFd(fd: Int)

    /**
     * Clear the active USB DAC file descriptor upon unplug.
     * Triggers fallback to AAudio.
     */
    external fun clearUsbDevice()

    /**
     * Get the currently registered USB file descriptor (-1 if none).
     */
    external fun getActiveUsbFd(): Int

    /**
     * Check if a bit-perfect USB DAC is currently active.
     */
    external fun isUsbDacActive(): Boolean
}
