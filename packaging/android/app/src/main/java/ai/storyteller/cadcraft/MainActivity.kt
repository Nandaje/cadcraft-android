package ai.storyteller.cadcraft

import android.Manifest
import android.app.Activity
import android.app.NativeActivity
import android.content.Intent
import android.content.pm.PackageManager
import android.media.MediaScannerConnection
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.provider.Settings
import java.io.File

/**
 * Android host Activity for CADCraft (`libcadcraft_android.so`).
 *
 * Bridges:
 * 1. Runtime storage permissions (`READ_EXTERNAL_STORAGE`, `WRITE_EXTERNAL_STORAGE`,
 *    `READ_MEDIA_IMAGES`, `MANAGE_EXTERNAL_STORAGE`) for importing and exporting CAD drawings,
 *    templates, xrefs, plot styles, linetypes, and PDFs.
 * 2. Storage Access Framework (SAF) system pickers (`ACTION_OPEN_DOCUMENT`,
 *    `ACTION_CREATE_DOCUMENT`, `ACTION_OPEN_DOCUMENT_TREE`).
 * 3. Incoming `ACTION_VIEW` / `ACTION_EDIT` / `ACTION_SEND` file intents.
 * 4. `MediaScannerConnection` indexing and SAF `content://` OutputStream synchronization so
 *    saved/exported DXF, DWG, SVG, PDF, and PNG files appear immediately in Android Files / Gallery.
 */
class MainActivity : NativeActivity() {

    companion object {
        private const val REQ_STORAGE_PERMISSIONS = 1001
        private const val REQ_ALL_FILES_ACCESS = 1002
        private const val REQ_SAF_OPEN = 2001
        private const val REQ_SAF_SAVE = 2002
        private const val REQ_SAF_FOLDER = 2003

        init {
            System.loadLibrary("cadcraft_android")
        }

        @JvmStatic
        external fun nativeOnStoragePermissionResult(
            readGranted: Boolean,
            writeGranted: Boolean,
            allFilesAccess: Boolean,
            sdkInt: Int
        )

        @JvmStatic
        external fun nativeDeliverFile(
            fileName: String,
            fileBytes: ByteArray
        )

        @JvmStatic
        external fun nativeDeliverSafOpenFile(
            fileName: String,
            fileBytes: ByteArray
        )

        @JvmStatic
        external fun nativeDeliverSafSavePath(
            savePath: String
        )

        @JvmStatic
        external fun nativeDeliverSafFolderPath(
            folderPath: String
        )
    }

    @Volatile
    private var pendingSafSaveUri: Uri? = null

    @Volatile
    private var pendingSafSaveStagedPath: String? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        reportStoragePermissions()
        handleIncomingIntent(intent)
    }

    override fun onNewIntent(intent: Intent?) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleIncomingIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        reportStoragePermissions()
    }

    /**
     * Called from Rust via JNI (`PlatformBridge::request_permissions`) or at startup.
     */
    @Suppress("unused")
    fun requestStoragePermissions(allFiles: Boolean) {
        if (allFiles && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            if (!Environment.isExternalStorageManager()) {
                try {
                    val uri = Uri.parse("package:$packageName")
                    val intent = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, uri)
                    startActivityForResult(intent, REQ_ALL_FILES_ACCESS)
                    return
                } catch (_: Exception) {
                    val intent = Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)
                    startActivityForResult(intent, REQ_ALL_FILES_ACCESS)
                    return
                }
            }
        }

        val permsToRequest = mutableListOf<String>()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            if (checkSelfPermission(Manifest.permission.READ_MEDIA_IMAGES) != PackageManager.PERMISSION_GRANTED) {
                permsToRequest.add(Manifest.permission.READ_MEDIA_IMAGES)
            }
        } else {
            if (checkSelfPermission(Manifest.permission.READ_EXTERNAL_STORAGE) != PackageManager.PERMISSION_GRANTED) {
                permsToRequest.add(Manifest.permission.READ_EXTERNAL_STORAGE)
            }
            if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.Q &&
                checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) != PackageManager.PERMISSION_GRANTED
            ) {
                permsToRequest.add(Manifest.permission.WRITE_EXTERNAL_STORAGE)
            }
        }

        if (permsToRequest.isNotEmpty()) {
            requestPermissions(permsToRequest.toTypedArray(), REQ_STORAGE_PERMISSIONS)
        } else {
            reportStoragePermissions()
        }
    }

    /**
     * Called from Rust via JNI (`PlatformBridge::launch_saf_open`).
     */
    @Suppress("unused")
    fun openSafFilePicker(multiple: Boolean) {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
            putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple)
        }
        startActivityForResult(intent, REQ_SAF_OPEN)
    }

    /**
     * Called from Rust via JNI (`PlatformBridge::launch_saf_save`).
     */
    @Suppress("unused")
    fun openSafSavePicker(suggestedName: String) {
        val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = mimeForName(suggestedName)
            putExtra(Intent.EXTRA_TITLE, suggestedName)
        }
        startActivityForResult(intent, REQ_SAF_SAVE)
    }

    /**
     * Called from Rust via JNI (`PlatformBridge::launch_saf_folder`).
     */
    @Suppress("unused")
    fun openSafFolderPicker() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
        startActivityForResult(intent, REQ_SAF_FOLDER)
    }

    /**
     * Called from Rust via JNI (`PlatformBridge::scan_media_file`) after a save or export finishes.
     * Also flushes any staged SAF save file into the user-picked `content://` URI if the URI did not
     * map to a direct filesystem path.
     */
    @Suppress("unused")
    fun scanMediaFile(path: String) {
        val safUri = pendingSafSaveUri
        val stagedPath = pendingSafSaveStagedPath
        if (safUri != null && stagedPath != null && path == stagedPath) {
            try {
                val src = File(path)
                if (src.isFile) {
                    contentResolver.openOutputStream(safUri, "wt")?.use { out ->
                        src.inputStream().use { input -> input.copyTo(out) }
                    }
                }
            } catch (_: Exception) {
            } finally {
                pendingSafSaveUri = null
                pendingSafSaveStagedPath = null
            }
        }
        MediaScannerConnection.scanFile(this, arrayOf(path), null, null)
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        reportStoragePermissions()
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        when (requestCode) {
            REQ_ALL_FILES_ACCESS -> reportStoragePermissions()
            REQ_SAF_OPEN -> {
                if (resultCode != Activity.RESULT_OK || data == null) {
                    nativeDeliverSafOpenFile("", ByteArray(0))
                    return
                }
                val clip = data.clipData
                if (clip != null && clip.itemCount > 0) {
                    val firstUri = clip.getItemAt(0).uri
                    deliverSafUriToOpen(firstUri)
                    for (i in 1 until clip.itemCount) {
                        deliverUriToInbox(clip.getItemAt(i).uri)
                    }
                } else {
                    val uri = data.data
                    if (uri != null) {
                        deliverSafUriToOpen(uri)
                    } else {
                        nativeDeliverSafOpenFile("", ByteArray(0))
                    }
                }
            }
            REQ_SAF_SAVE -> {
                if (resultCode != Activity.RESULT_OK || data?.data == null) {
                    nativeDeliverSafSavePath("")
                    return
                }
                val uri = data.data!!
                val displayName = resolveDisplayName(uri) ?: "Drawing1.dxf"
                val directFile = resolveDirectFileFromUri(uri)
                if (directFile != null) {
                    pendingSafSaveUri = null
                    pendingSafSaveStagedPath = null
                    nativeDeliverSafSavePath(directFile.absolutePath)
                } else {
                    val exportsDir = File(getExternalFilesDir(null) ?: filesDir, "Exports")
                    exportsDir.mkdirs()
                    val stagedFile = File(exportsDir, displayName)
                    pendingSafSaveUri = uri
                    pendingSafSaveStagedPath = stagedFile.absolutePath
                    nativeDeliverSafSavePath(stagedFile.absolutePath)
                }
            }
            REQ_SAF_FOLDER -> {
                if (resultCode != Activity.RESULT_OK || data?.data == null) {
                    nativeDeliverSafFolderPath("")
                    return
                }
                val uri = data.data!!
                val folder = resolveDirectFolderFromTreeUri(uri)
                    ?: File(getExternalFilesDir(null) ?: filesDir, "Exports").apply { mkdirs() }
                nativeDeliverSafFolderPath(folder.absolutePath)
            }
        }
    }

    private fun reportStoragePermissions() {
        val allFiles = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            Environment.isExternalStorageManager()
        } else {
            false
        }
        val readGranted = allFiles || if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            checkSelfPermission(Manifest.permission.READ_MEDIA_IMAGES) == PackageManager.PERMISSION_GRANTED
        } else {
            checkSelfPermission(Manifest.permission.READ_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED
        }
        val writeGranted = allFiles || if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.Q) {
            checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED
        } else {
            Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS).canWrite()
        }
        nativeOnStoragePermissionResult(readGranted, writeGranted, allFiles, Build.VERSION.SDK_INT)
    }

    private fun handleIncomingIntent(intent: Intent?) {
        if (intent == null) return
        when (intent.action) {
            Intent.ACTION_VIEW, Intent.ACTION_EDIT -> {
                intent.data?.let { uri -> deliverUriToInbox(uri) }
            }
            Intent.ACTION_SEND -> {
                @Suppress("DEPRECATION")
                val uri = intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM) ?: intent.data
                if (uri != null) {
                    deliverUriToInbox(uri)
                }
            }
        }
    }

    private fun deliverSafUriToOpen(uri: Uri) {
        try {
            val name = resolveDisplayName(uri) ?: uri.lastPathSegment ?: "Drawing.dxf"
            val bytes = contentResolver.openInputStream(uri)?.use { it.readBytes() }
            if (bytes != null) {
                nativeDeliverSafOpenFile(name, bytes)
            } else {
                nativeDeliverSafOpenFile("", ByteArray(0))
            }
        } catch (_: Exception) {
            nativeDeliverSafOpenFile("", ByteArray(0))
        }
    }

    private fun deliverUriToInbox(uri: Uri) {
        try {
            val name = resolveDisplayName(uri) ?: uri.lastPathSegment ?: "Drawing.dxf"
            val bytes = contentResolver.openInputStream(uri)?.use { it.readBytes() } ?: return
            nativeDeliverFile(name, bytes)
        } catch (_: Exception) {
        }
    }

    private fun resolveDisplayName(uri: Uri): String? {
        if (uri.scheme == "content") {
            contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (idx >= 0) return cursor.getString(idx)
                }
            }
        }
        return uri.lastPathSegment?.substringAfterLast('/')
    }

    private fun resolveDirectFileFromUri(uri: Uri): File? {
        if (uri.scheme == "file") {
            return uri.path?.let { File(it) }
        }
        if (DocumentsContract.isDocumentUri(this, uri)) {
            val docId = DocumentsContract.getDocumentId(uri)
            if (docId.startsWith("primary:")) {
                val rel = docId.removePrefix("primary:")
                val f = File(Environment.getExternalStorageDirectory(), rel)
                if (f.parentFile?.canWrite() == true) return f
            }
        }
        return null
    }

    private fun resolveDirectFolderFromTreeUri(uri: Uri): File? {
        val treeId = DocumentsContract.getTreeDocumentId(uri)
        if (treeId.startsWith("primary:")) {
            val rel = treeId.removePrefix("primary:")
            val f = File(Environment.getExternalStorageDirectory(), rel)
            if (f.isDirectory) return f
        }
        return null
    }

    private fun mimeForName(name: String): String {
        return when (name.substringAfterLast('.', "").lowercase()) {
            "dxf" -> "application/dxf"
            "dwg" -> "application/acad"
            "dwt" -> "application/x-autocad-template"
            "svg", "svgz" -> "image/svg+xml"
            "pdf" -> "application/pdf"
            "png" -> "image/png"
            "lsp", "scr", "lin", "pat" -> "text/plain"
            else -> "application/octet-stream"
        }
    }
}
