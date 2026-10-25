package dev.audiomon

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import android.os.PowerManager

/** Foreground service that keeps the engine alive with the screen off. */
class AudioService : Service() {
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.notif_channel), NotificationManager.IMPORTANCE_LOW),
        )
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = Notification.Builder(this, CHANNEL)
            .setContentTitle("audiomon")
            .setContentText(getString(R.string.notif_text))
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setContentIntent(open)
            .setOngoing(true)
            .build()
        startForeground(1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)

        wakeLock = getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "audiomon:stream")
            .apply { acquire() }
        Native.start()
        running = true
        takeAccessoryFd()?.let(Native::startAccessory)
    }

    override fun onDestroy() {
        running = false
        Native.stop()
        wakeLock?.release()
        super.onDestroy()
    }

    companion object {
        private const val CHANNEL = "stream"

        @Volatile
        var running = false
            private set

        /** Accessory fd opened by the activity, waiting for the service to start. */
        @Volatile
        var pendingAccessoryFd = -1

        fun takeAccessoryFd(): Int? {
            val fd = pendingAccessoryFd
            pendingAccessoryFd = -1
            return fd.takeIf { it >= 0 }
        }
    }
}
