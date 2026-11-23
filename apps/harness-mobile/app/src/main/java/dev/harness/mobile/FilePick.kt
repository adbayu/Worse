package dev.harness.mobile

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.MediaStore

/** Most attachments one message may carry (chat::limits MAX_ITEMS). */
private const val MAX_PICK = 10

/** True when every `accept` type is an image type (the composer's "Photos" entry). */
internal fun onlyImages(acceptTypes: Array<String>): Boolean {
    val types = acceptTypes.flatMap { it.split(',') }.map { it.trim() }.filter { it.isNotEmpty() }
    return types.isNotEmpty() && types.all { it.startsWith("image/") }
}

/** The system photo picker (Android 13+), else the gallery through GET_CONTENT. */
internal fun photoIntent(multiple: Boolean): Intent {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        return Intent(MediaStore.ACTION_PICK_IMAGES).apply {
            if (multiple) {
                putExtra(MediaStore.EXTRA_PICK_IMAGES_MAX, minOf(MAX_PICK, MediaStore.getPickImagesMaxLimit()))
            }
        }
    }
    return Intent(Intent.ACTION_GET_CONTENT).apply {
        type = "image/*"
        addCategory(Intent.CATEGORY_OPENABLE)
        if (multiple) putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
    }
}

/** The system file manager (Documents UI) for any file type. */
internal fun filesIntent(multiple: Boolean): Intent =
    Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
        type = "*/*"
        addCategory(Intent.CATEGORY_OPENABLE)
        if (multiple) putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
    }

/** Picked uris from either picker (single `data` or multi `clipData`); null when cancelled. */
internal fun pickedUris(resultCode: Int, data: Intent?): Array<Uri>? {
    if (resultCode != Activity.RESULT_OK || data == null) return null
    val clip = data.clipData
    if (clip != null && clip.itemCount > 0) {
        return Array(clip.itemCount) { clip.getItemAt(it).uri }
    }
    return data.data?.let { arrayOf(it) }
}
