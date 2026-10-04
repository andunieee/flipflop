package dev.flipflop.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import uniffi.flipflop.AppRow
import uniffi.flipflop.listApps

/** A rename / delete dialog: kind is "peer" or "group". */
class Edit(val kind: String, val id: String, val original: String, text: String) {
    var text by mutableStateOf(text)
}

/** What only the UI cares about: where we are, the open tab, dialogs. */
class Nav(val backend: Backend) {
    /** "home" | "groups" | "group" | "peers" | "add" | "settings" */
    var page by mutableStateOf("home")
        private set
    /** "files" | "texts" | "pad" | "apps" | "info" */
    var groupTab by mutableStateOf("files")
    var edit by mutableStateOf<Edit?>(null)
    var apps by mutableStateOf<List<AppRow>>(emptyList())
    var appPickerOpen by mutableStateOf(false)
    var inviteChoice by mutableStateOf(0)

    private val app get() = backend.app

    /** Where leaving a group goes: the groups list on phones, Home on the desktop (which has no list page). */
    val listPage get() = if (backend.touch) "groups" else "home"

    fun go(page: String) {
        this.page = page
        app.pageChanged(page)
    }

    /** Open a group's page, on [tab] if given. */
    fun openGroup(id: String, tab: String? = null) {
        val implied = backend.state.value?.peers?.any { it.groupId == id } == true
        app.openGroup(id)
        groupTab = tab ?: if (implied) "info" else "files"
        inviteChoice = 0
        go("group")
    }

    fun closeGroup() {
        app.openGroup(null)
        go(listPage)
    }

    fun newGroup() {
        val id = app.newGroup() ?: return
        groupTab = "files"
        go("group")
    }

    /** A new guest link: its group opens on the page that shows the link. */
    fun newGuestLink() {
        app.newGuestLink(false) ?: return
        groupTab = "info"
        go("group")
    }

    /** Android Back: dialog → scan → group → Home. Returns false when nothing is left to close. */
    fun back(scanning: Boolean): Boolean {
        when {
            appPickerOpen -> appPickerOpen = false
            edit != null -> edit = null
            scanning -> app.cancelScan()
            page == "group" -> closeGroup()
            page != "home" -> go("home")
            else -> return false
        }
        return true
    }

    fun pick(kind: String) {
        if (kind == "app") {
            apps = listApps()
            appPickerOpen = true
        } else {
            app.pick(kind)
        }
    }
}
