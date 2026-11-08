package com.porgous.mobile.data

import android.content.Context

/**
 * Session state lives in app-private SharedPreferences. Android sandboxes this
 * storage to the app; the session cookie is never written to external storage.
 */
class SessionStore(context: Context) {
    private val prefs = context.getSharedPreferences("porgous_session", Context.MODE_PRIVATE)

    var baseUrl: String?
        get() = prefs.getString(KEY_BASE_URL, null)
        set(value) = prefs.edit().putString(KEY_BASE_URL, value).apply()

    var cookie: String?
        get() = prefs.getString(KEY_COOKIE, null)
        set(value) = prefs.edit().putString(KEY_COOKIE, value).apply()

    var csrf: String?
        get() = prefs.getString(KEY_CSRF, null)
        set(value) = prefs.edit().putString(KEY_CSRF, value).apply()

    var username: String?
        get() = prefs.getString(KEY_USERNAME, null)
        set(value) = prefs.edit().putString(KEY_USERNAME, value).apply()

    fun clearSession() {
        prefs.edit()
            .remove(KEY_COOKIE)
            .remove(KEY_CSRF)
            .remove(KEY_USERNAME)
            .apply()
    }

    private companion object {
        const val KEY_BASE_URL = "base_url"
        const val KEY_COOKIE = "cookie"
        const val KEY_CSRF = "csrf"
        const val KEY_USERNAME = "username"
    }
}
