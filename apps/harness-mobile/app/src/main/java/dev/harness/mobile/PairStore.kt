package dev.harness.mobile

import android.content.Context
import android.net.Uri
import android.util.Log
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Stores the paired Harness origin (`https://<host>[:port]`) in EncryptedSharedPreferences.
 *
 * The device token itself lives in the WebView cookie jar (the pair link is opened once, the page
 * posts the token to `/api/v1/pair` and the server sets `harness_remote`), so only the origin is
 * persisted here.
 */
object PairStore {

    private const val TAG = "HarnessPair"
    private const val FILE = "harness_pair"
    private const val KEY_ORIGIN = "origin"

    /** A parsed pair link: the origin to persist plus the full URL to open once. */
    data class PairLink(val origin: String, val url: String) {
        val host: String get() = Uri.parse(origin).host.orEmpty()
    }

    private fun prefs(context: Context) =
        EncryptedSharedPreferences.create(
            context,
            FILE,
            MasterKey.Builder(context).setKeyScheme(MasterKey.KeyScheme.AES256_GCM).build(),
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )

    fun origin(context: Context): String? = runCatching {
        prefs(context).getString(KEY_ORIGIN, null)
    }.onFailure { Log.w(TAG, "read failed: ${it.javaClass.simpleName}") }.getOrNull()

    fun saveOrigin(context: Context, origin: String) {
        runCatching { prefs(context).edit().putString(KEY_ORIGIN, origin).apply() }
            .onFailure { Log.w(TAG, "write failed: ${it.javaClass.simpleName}") }
    }

    fun clear(context: Context) {
        runCatching { prefs(context).edit().clear().apply() }
            .onFailure { Log.w(TAG, "clear failed: ${it.javaClass.simpleName}") }
    }

    /**
     * Accepts `https://<host>/p#t=<token>`, and tolerates `https://<host>`, `https://<host>/` and
     * `/p` without a fragment (the page then asks for the short code). Rejects anything that is not
     * https with a host.
     */
    fun parse(raw: String?): PairLink? {
        val text = raw?.trim().orEmpty()
        if (text.isEmpty()) return null
        val uri = runCatching { Uri.parse(text) }.getOrNull() ?: return null
        val scheme = uri.scheme?.lowercase()
        if (scheme != "https") return null
        val host = uri.host?.trim().orEmpty()
        if (host.isEmpty()) return null
        val port = if (uri.port > 0 && uri.port != 443) ":${uri.port}" else ""
        // 中继链接 `https://<relay>/<device-id>#k=…` 必须保留设备路径，否则重启后会打开中继首页。
        val device = uri.path.orEmpty().trim('/').takeIf { RELAY_ID.matches(it) }
        val origin = if (device != null) "https://$host$port/$device" else "https://$host$port"
        return PairLink(origin = origin, url = text)
    }

    /** Device id path of a Remote Control relay link (docs/plans/remote-relay.md §1). */
    private val RELAY_ID = Regex("^[a-z2-7]{16}$")
}
