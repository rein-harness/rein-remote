package dev.reinharness.remote

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.hardware.input.InputManager
import android.os.Bundle
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView

/** Pairings live in app-private storage that isn't backed up: the core's keys never leave the device. */
object Core {
    @Volatile private var ready = false

    fun setUp(c: Context) {
        if (ready) return
        synchronized(this) {
            if (ready) return
            val dir = java.io.File(c.noBackupFilesDir, "rein-remote")
            dir.mkdirs()
            Native.setHome(dir.path)
            ready = true
        }
    }
}

/**
 * The remote screen, full screen, with a small bar: disconnect, the keyboard, the numbers.
 *
 * Extras (also for tests and measuring, `adb shell am start -n dev.reinharness.remote/.StreamActivity
 * -e url URL …`): url, name, pair (a pairing code), measure, selftest, seconds, audio, webtransport,
 * hevc (offer HEVC even where it's decoded in software), decoder (one by name: c2.android.avc.decoder).
 */
class StreamActivity : Activity(), InputManager.InputDeviceListener {
    lateinit var controller: StreamController
        private set
    private lateinit var view: StreamView
    private lateinit var stats: TextView
    private lateinit var title: TextView
    private var showStats = false
    @Volatile var selfTestLine: String? = null
        private set
    @Volatile var failed: String? = null
        private set

    override fun onCreate(saved: Bundle?) {
        super.onCreate(saved)
        Core.setUp(this)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        @Suppress("DEPRECATION")
        window.decorView.systemUiVisibility = View.SYSTEM_UI_FLAG_FULLSCREEN or View.SYSTEM_UI_FLAG_HIDE_NAVIGATION or
            View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY or View.SYSTEM_UI_FLAG_LAYOUT_STABLE
        val x = intent
        val url = x.getStringExtra(EXTRA_URL) ?: return finish()
        val measure = x.getBooleanExtra("measure", false) || x.getBooleanExtra("selftest", false)
        showStats = measure

        view = StreamView(this)
        controller = StreamController(this, view)
        controller.measure = measure
        // A decoder by name (MediaCodecList's), in place of the best one.
        controller.decoder.prefer = x.getStringExtra("decoder")
        val root = FrameLayout(this)
        root.addView(view, FrameLayout.LayoutParams(-1, -1))
        val bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setBackgroundColor(0x88000000.toInt())
            setPadding(dp(4), dp(2), dp(8), dp(2))
            gravity = Gravity.CENTER_VERTICAL
        }
        fun button(label: String, desc: String, f: () -> Unit) = Button(this, null, android.R.attr.borderlessButtonStyle).apply {
            text = label
            contentDescription = desc
            setTextColor(Color.WHITE)
            minWidth = 0
            minimumWidth = 0
            setPadding(dp(10), 0, dp(10), 0)
            isFocusable = false
            setOnClickListener { f() }
            bar.addView(this)
        }
        button("✕", "Disconnect") { finish() }
        button("⌨", "Keyboard") { view.toggleKeyboard() }
        button("ⓘ", "Stats") {
            showStats = !showStats
            refresh()
        }
        title = TextView(this).apply {
            setTextColor(Color.WHITE)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            text = x.getStringExtra("name") ?: url.substringBefore('?')
        }
        bar.addView(title)
        val top = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        top.addView(bar, LinearLayout.LayoutParams(-2, -2))
        stats = TextView(this).apply {
            setTextColor(Color.WHITE)
            setBackgroundColor(0x88000000.toInt())
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
            typeface = android.graphics.Typeface.MONOSPACE
            setPadding(dp(8), dp(4), dp(8), dp(4))
            contentDescription = "Stats"
        }
        top.addView(stats, LinearLayout.LayoutParams(-2, -2).apply { topMargin = dp(4) })
        root.addView(top, FrameLayout.LayoutParams(-2, -2, Gravity.TOP or Gravity.START).apply {
            leftMargin = dp(8)
            topMargin = dp(8)
        })
        setContentView(root)
        view.requestFocus()
        controller.onState = { refresh() }
        getSystemService(InputManager::class.java).registerInputDeviceListener(this, null)

        val hevc = x.getBooleanExtra("hevc", false) || Decoders.hardware(Codec.H265.mime)
        val o = ConnectOptions(
            webtransport = x.getBooleanExtra("webtransport", true),
            audio = x.getBooleanExtra("audio", true),
            codecs = if (hevc) listOf(Codec.H265, Codec.H264) else listOf(Codec.H264),
            hardware = Decoders.hardware(Codec.H264.mime),
            refreshMhz = (refreshRate() * 1000).toInt(),
        )
        val pair = x.getStringExtra("pair")
        val selfTest = x.getBooleanExtra("selftest", false)
        val seconds = x.getDoubleExtra("seconds", 10.0)
        Thread({
            try {
                controller.connect(url, pair, o)
                if (selfTest) {
                    val line = SelfTest.run(controller, seconds)
                    selfTestLine = line
                    runOnUiThread { title.text = line }
                }
            } catch (e: Exception) {
                Log.w(TAG, "connect: $e")
                failed = e.message ?: "$e"
                runOnUiThread {
                    title.text = "couldn't connect: ${e.message}"
                    stats.visibility = View.GONE
                }
            }
        }, "rein-remote connect").start()
    }

    @Suppress("DEPRECATION")
    private fun refreshRate(): Float =
        (if (android.os.Build.VERSION.SDK_INT >= 30) display?.refreshRate else windowManager.defaultDisplay.refreshRate) ?: 60f

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    private fun refresh() {
        if (failed != null) return
        title.text = selfTestLine ?: controller.serverName.ifEmpty { title.text.toString() }
        stats.visibility = if (showStats) View.VISIBLE else View.GONE
        stats.text = if (controller.state == "streaming") controller.overlay() else controller.state
    }

    override fun dispatchKeyEvent(e: KeyEvent): Boolean {
        if (::controller.isInitialized) {
            if (controller.gamepads.key(e)) return true
            // Keys the soft keyboard's view didn't take (it's not focused): to the server.
            if (!view.keyboard.hasFocus() && view.key(e)) return true
        }
        return super.dispatchKeyEvent(e)
    }

    override fun dispatchGenericMotionEvent(e: MotionEvent): Boolean {
        if (::controller.isInitialized && controller.gamepads.motion(e)) return true
        return super.dispatchGenericMotionEvent(e)
    }

    override fun onInputDeviceAdded(id: Int) {}
    override fun onInputDeviceChanged(id: Int) {}
    override fun onInputDeviceRemoved(id: Int) {
        if (::controller.isInitialized) controller.gamepads.removed(id)
    }

    override fun onDestroy() {
        getSystemService(InputManager::class.java).unregisterInputDeviceListener(this)
        if (::controller.isInitialized) controller.close()
        super.onDestroy()
    }

    companion object {
        const val EXTRA_URL = "url"

        fun intent(c: Context, url: String, name: String, pair: String?): Intent =
            Intent(c, StreamActivity::class.java).putExtra(EXTRA_URL, url).putExtra("name", name).apply {
                if (pair != null) putExtra("pair", pair)
            }
    }
}
