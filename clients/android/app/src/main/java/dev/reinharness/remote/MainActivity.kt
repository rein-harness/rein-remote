package dev.reinharness.remote

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.graphics.Typeface
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.InputType
import android.util.Log
import android.util.TypedValue
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import org.json.JSONArray
import org.json.JSONObject
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress

/** A server found on this network (`_rein-remote._tcp`, what `rein-remote serve` advertises when it listens beyond its own machine). */
data class FoundServer(val name: String, val id: String, val url: String)

/**
 * Finds servers with Android's own DNS-SD (NsdManager), which needs no permission and no multicast
 * lock; each found service is resolved (one at a time: older Androids refuse two at once) to its
 * address, port and TXT record.
 */
class Discovery(context: Context, private val onChange: (List<FoundServer>) -> Unit) {
    private val nsd = context.getSystemService(NsdManager::class.java)
    private val main = Handler(Looper.getMainLooper())
    private val found = LinkedHashMap<String, FoundServer>()
    private val toResolve = ArrayDeque<NsdServiceInfo>()
    private var resolving = false
    private var listener: NsdManager.DiscoveryListener? = null

    fun start() {
        if (listener != null) return
        val l = object : NsdManager.DiscoveryListener {
            override fun onServiceFound(info: NsdServiceInfo) = main.post { queue(info) }.let {}
            override fun onServiceLost(info: NsdServiceInfo) = main.post {
                found.remove(info.serviceName)
                onChange(found.values.toList())
            }.let {}
            override fun onDiscoveryStarted(type: String) {}
            override fun onDiscoveryStopped(type: String) {}
            override fun onStartDiscoveryFailed(type: String, code: Int) = Log.w(TAG, "discovery failed: $code").let {}
            override fun onStopDiscoveryFailed(type: String, code: Int) {}
        }
        listener = l
        nsd.discoverServices(TYPE, NsdManager.PROTOCOL_DNS_SD, l)
    }

    fun stop() {
        listener?.let { runCatching { nsd.stopServiceDiscovery(it) } }
        listener = null
    }

    private fun queue(info: NsdServiceInfo) {
        toResolve.addLast(info)
        next()
    }

    @Suppress("DEPRECATION")
    private fun next() {
        if (resolving) return
        val info = toResolve.removeFirstOrNull() ?: return
        resolving = true
        nsd.resolveService(info, object : NsdManager.ResolveListener {
            override fun onResolveFailed(i: NsdServiceInfo, code: Int) = main.post {
                resolving = false
                next()
            }.let {}

            override fun onServiceResolved(i: NsdServiceInfo) = main.post {
                resolving = false
                val txt = ServerTxt.of(i.attributes ?: emptyMap())
                val host = address(i)
                if (host != null) {
                    found[i.serviceName] = FoundServer(i.serviceName, txt.id, "http://$host:${i.port}/")
                    onChange(found.values.toList())
                }
                next()
            }.let {}
        })
    }

    /** An IPv4 address first (the core takes those everywhere), else IPv6 (not link-local). */
    @Suppress("DEPRECATION")
    private fun address(i: NsdServiceInfo): String? {
        val all: List<InetAddress> = if (Build.VERSION.SDK_INT >= 34) i.hostAddresses else listOfNotNull(i.host)
        all.firstOrNull { it is Inet4Address }?.let { return it.hostAddress }
        return all.firstOrNull { it is Inet6Address && !it.isLinkLocalAddress }?.let { "[${it.hostAddress?.substringBefore('%')}]" }
    }

    companion object {
        const val TYPE = "_rein-remote._tcp"
    }
}

/** Servers kept on this device: ones paired with (by fingerprint) and links added by hand. */
data class SavedServer(val name: String, val url: String, val serverId: String?) {
    val key: String get() = serverId ?: url
}

class SavedServers(context: Context) {
    private val prefs = context.getSharedPreferences("servers", Context.MODE_PRIVATE)
    var list: List<SavedServer> = load()
        private set

    private fun load(): List<SavedServer> = try {
        val a = JSONArray(prefs.getString("list", "[]"))
        (0 until a.length()).map { i ->
            val o = a.getJSONObject(i)
            SavedServer(o.getString("name"), o.getString("url"), o.optString("id").ifEmpty { null })
        }
    } catch (e: Exception) {
        emptyList()
    }

    fun add(s: SavedServer) {
        list = listOf(s) + list.filter { it.key != s.key }
        save()
    }

    fun remove(s: SavedServer) {
        list = list.filter { it.key != s.key }
        save()
    }

    private fun save() {
        val a = JSONArray()
        for (s in list) a.put(JSONObject().put("name", s.name).put("url", s.url).put("id", s.serverId ?: ""))
        prefs.edit().putString("list", a.toString()).apply()
    }
}

/** The list of computers: found on this network, saved, or added by a link. */
class MainActivity : Activity() {
    private lateinit var discovery: Discovery
    private lateinit var saved: SavedServers
    private lateinit var list: LinearLayout
    private var found: List<FoundServer> = emptyList()

    override fun onCreate(s: Bundle?) {
        super.onCreate(s)
        Core.setUp(this)
        saved = SavedServers(this)
        list = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(16), dp(16), dp(16))
        }
        setContentView(ScrollView(this).apply { addView(list) })
        discovery = Discovery(this) {
            found = it
            render()
        }
        render()
    }

    override fun onResume() {
        super.onResume()
        discovery.start()
        render()
    }

    override fun onPause() {
        discovery.stop()
        super.onPause()
    }

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    private fun header(t: String) = TextView(this).apply {
        text = t
        setTypeface(typeface, Typeface.BOLD)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
        setPadding(0, dp(16), 0, dp(4))
    }

    private fun note(t: String) = TextView(this).apply {
        text = t
        alpha = 0.7f
    }

    private fun item(title: String, sub: String, click: () -> Unit, long: (() -> Unit)? = null) =
        Button(this).apply {
            text = "$title\n$sub"
            isAllCaps = false
            gravity = android.view.Gravity.START or android.view.Gravity.CENTER_VERTICAL
            setOnClickListener { click() }
            long?.let { l -> setOnLongClickListener { l(); true } }
        }

    private fun render() {
        list.removeAllViews()
        list.addView(TextView(this).apply {
            text = getString(R.string.app_name)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 24f)
        })
        list.addView(header("On this network"))
        if (found.isEmpty()) list.addView(note("Looking for computers running rein-remote…"))
        for (f in found) {
            val paired = f.id.isNotEmpty() && Native.isPaired(f.id)
            list.addView(item(f.name, if (paired) "paired" else "pair with its code", { choose(f) }))
        }
        val away = saved.list.filter { s -> found.none { it.id.isNotEmpty() && it.id == s.serverId } }
        if (away.isNotEmpty()) {
            list.addView(header("Saved"))
            for (s in away) {
                list.addView(item(s.name, s.url.substringBefore('?'), { open(s.url, s.name, null) }, {
                    AlertDialog.Builder(this).setMessage("Forget ${s.name}?")
                        .setPositiveButton("Forget") { _, _ ->
                            saved.remove(s)
                            render()
                        }.setNegativeButton("Cancel", null).show()
                }))
            }
        }
        list.addView(Button(this).apply {
            text = "Add a computer"
            setOnClickListener { addDialog() }
        })
        list.addView(note("\nA link `rein-remote serve` prints, a relay's link, or a computer's address and the code it shows. Core ${Native.version()}."))
    }

    private fun choose(f: FoundServer) {
        if (f.id.isNotEmpty() && Native.isPaired(f.id)) {
            saved.add(SavedServer(f.name, f.url, f.id))
            open(f.url, f.name, null)
        } else {
            codeDialog("Pair with ${f.name}") { code ->
                saved.add(SavedServer(f.name, f.url, f.id.ifEmpty { null }))
                open(f.url, f.name, code)
            }
        }
    }

    private fun codeDialog(title: String, done: (String) -> Unit) {
        val code = EditText(this).apply {
            hint = "123-456"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        }
        AlertDialog.Builder(this).setTitle(title).setMessage("The code it shows").setView(code)
            .setPositiveButton("Connect") { _, _ -> if (code.text.isNotBlank()) done(code.text.toString().trim()) }
            .setNegativeButton("Cancel", null).show()
    }

    private fun addDialog() {
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(20), 0, dp(20), 0)
        }
        val link = EditText(this).apply {
            hint = "http://computer:7979/?token=…"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
        }
        val code = EditText(this).apply {
            hint = "Its pairing code (if no token)"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        }
        box.addView(link)
        box.addView(code)
        AlertDialog.Builder(this).setTitle("Add a computer").setView(box)
            .setPositiveButton("Connect") { _, _ ->
                val p = PairLink.parse(link.text.toString()) ?: return@setPositiveButton
                val name = java.net.URI(p.url).host ?: "server"
                val c = code.text.toString().trim().ifEmpty { p.code }
                // Only links without a secret in them are kept (a token is this run's, a relay's key is private).
                if (!p.url.contains("token=") && !p.url.contains('#')) saved.add(SavedServer(name, p.url, p.id))
                open(p.url, name, c)
            }
            .setNegativeButton("Cancel", null).show()
    }

    private fun open(url: String, name: String, pair: String?) {
        startActivity(StreamActivity.intent(this, url, name, pair))
    }
}
