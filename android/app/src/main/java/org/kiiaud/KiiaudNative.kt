package org.kiiaud

/**
 * JNI Bridge to kiiaud's Pure Rust Engine (libkiiaud_output_android.so).
 */
object KiiaudNative {

    init {
        try {
            System.loadLibrary("kiiaud_output_android")
        } catch (e: UnsatisfiedLinkError) {
            e.printStackTrace()
        }
    }

    /**
     * Pass the active USB DAC file descriptor into Rust core.
     * Triggers dynamic switch to Direct USB UAC2 Bit-Perfect Backend.
     */
    external fun setUsbDeviceFd(fd: Int)

    /**
     * Clear the active USB DAC file descriptor upon unplug.
     * Triggers fallback to Oboe / AAudio.
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

