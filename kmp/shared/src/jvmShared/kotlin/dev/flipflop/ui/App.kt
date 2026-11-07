package dev.flipflop.ui

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.hoverable
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.imePadding
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import org.jetbrains.compose.resources.DrawableResource
import org.jetbrains.compose.resources.painterResource
import uniffi.flipflop.GroupRow
import uniffi.flipflop.PeerRow
import uniffi.flipflop.UiState

/** The whole app, over a running [Backend]. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun FlipflopApp(backend: Backend) {
    val state by backend.state.collectAsState()
    val s = state
    BoxWithConstraints(Modifier.fillMaxSize()) {
        FlipflopTheme(touch = backend.touch, narrow = maxWidth < 420.dp, theme = backend.theme) {
            val nav = remember(backend) { Nav(backend) }
            if (s == null) {
                Box(Modifier.fillMaxSize().background(Theme.c.windowBg))
            } else {
                Shell(s, nav)
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Shell(s: UiState, nav: Nav) {
    val c = Theme.c
    val backend = nav.backend
    val touch = backend.touch
    val scanView = s.scanning && !touch

    // Rust asks to go somewhere; a group that went away takes its page with it.
    LaunchedEffect(nav) { backend.navigation.collect { nav.go(it) } }
    LaunchedEffect(nav) { backend.drops.collect { nav.dropped(it) } }
    LaunchedEffect(s.openGroup, nav.page) {
        if (nav.page == "group" && s.openGroup.isEmpty()) nav.go(nav.listPage)
    }
    PlatformBackHandler(touch) { if (!nav.back(s.scanning)) { /* nothing left: let the system have it */ } }

    var toast by remember { mutableStateOf("") }
    LaunchedEffect(nav) {
        backend.notices.collect {
            toast = it
            delay(4000)
            if (toast == it) toast = ""
        }
    }

    Box(Modifier.fillMaxSize().background(c.windowBg)) {
        Column(Modifier.fillMaxSize().then(if (touch) Modifier.windowInsetsPadding(WindowInsets.safeDrawing) else Modifier)) {
            if (touch) AppBar(s)
            Row(Modifier.weight(1f).fillMaxWidth()) {
                if (!touch) Sidebar(s, nav)
                Box(Modifier.weight(1f).fillMaxHeight()) {
                    when {
                        scanView -> ScanView(nav)
                        nav.page == "home" -> HomePage(s, nav)
                        nav.page == "groups" -> GroupsPage(s, nav)
                        nav.page == "group" -> GroupPage(s, nav)
                        nav.page == "peers" -> PeersPage(s, nav)
                        nav.page == "add" -> AddPage(s, nav)
                        nav.page == "settings" -> SettingsPage(s, nav)
                    }
                }
            }
            if (touch && WindowInsets.ime.getBottom(LocalDensity.current) == 0) BottomNav(s, nav)
        }

        if (toast.isNotEmpty()) {
            Box(Modifier.fillMaxSize().padding(bottom = if (touch) 80.dp else 24.dp).windowInsetsPadding(WindowInsets.safeDrawing), contentAlignment = Alignment.BottomCenter) {
                Box(
                    Modifier.padding(horizontal = 16.dp).shadow(12.dp, RoundedCornerShape(Theme.d.radius)).clip(RoundedCornerShape(Theme.d.radius))
                        .background(c.textStrong).padding(horizontal = 16.dp, vertical = 10.dp),
                ) { Txt(toast, Theme.d.fontSm, c.windowBg, align = TextAlign.Center) }
            }
        }

        nav.edit?.let { EditDialog(it, nav) }
        if (nav.appPickerOpen) AppPicker(nav)
        backend.share?.let { SharePicker(s, it, nav) }
        if (backend.dropHover) DropHint(s, nav)
    }
}

@Composable
private fun NodeStatus(s: UiState) {
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
        Dot(s.nodeReady, 7.dp)
        Txt(
            if (!s.nodeReady) "Starting…" else if (s.myName.isEmpty()) "Online" else s.myName,
            Theme.d.fontXs, if (s.nodeReady) Theme.c.textMuted else Theme.c.warn, elide = true,
        )
    }
}

@Composable
private fun Logo(size: androidx.compose.ui.unit.Dp) {
    androidx.compose.foundation.Image(painterResource(Icons.logo), null, Modifier.size(size))
}

/** Phone app bar. */
@Composable
private fun AppBar(s: UiState) {
    Column(Modifier.fillMaxWidth().background(Theme.c.windowBg)) {
        Row(Modifier.fillMaxWidth().height(56.dp).padding(horizontal = Theme.d.pagePadding), horizontalArrangement = Arrangement.spacedBy(Theme.d.space3), verticalAlignment = Alignment.CenterVertically) {
            Logo(32.dp)
            Column(verticalArrangement = Arrangement.spacedBy(1.dp)) {
                Txt("flipflop", Theme.d.fontLg, Theme.c.textStrong, 700)
                NodeStatus(s)
            }
        }
        Divider()
    }
}

// ---- desktop sidebar -------------------------------------------------------------------------

private val SIDEBAR_MIN_WIDTH = 180.dp
private val SIDEBAR_MAX_WIDTH = 420.dp

@Composable
private fun SideRow(active: Boolean, onClick: () -> Unit, height: androidx.compose.ui.unit.Dp, content: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit) {
    val c = Theme.c
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    val hovered by source.collectIsHoveredAsState()
    // Fade to/from the hover colour's own transparent: Color.Transparent is black, and lerping from it flashes dark.
    val bg by animateColorAsState(if (active) c.accentSoft else if (pressed) c.chipBgHover else if (hovered) c.chipBg else c.chipBg.copy(alpha = 0f), tween(120))
    Row(
        Modifier.fillMaxWidth().height(height).clip(RoundedCornerShape(Theme.d.radius)).background(bg)
            .hoverable(source).pointerHoverIcon(PointerIcon.Hand).clickable(source, indication = null, onClick = onClick).padding(horizontal = Theme.d.space3),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) { content() }
}

@Composable
private fun CountBadge(n: Int, size: Int = 18) {
    Box(
        Modifier.heightIn(min = size.dp).widthIn(min = size.dp).clip(CircleShape).background(Theme.c.accent).padding(horizontal = 5.dp),
        contentAlignment = Alignment.Center,
    ) { Txt(n.toString(), if (size > 16) 11.sp else 10.sp, Color.White, 700) }
}

@Composable
private fun SideButton(label: String, icon: DrawableResource, active: Boolean, badge: Int = 0, onClick: () -> Unit) {
    SideRow(active, onClick, 36.dp) {
        Icon(icon, 18.dp, if (active) Theme.c.accentText else Theme.c.textMuted)
        Txt(label, Theme.d.fontMd, if (active) Theme.c.textStrong else Theme.c.text, if (active) 600 else 500, Modifier.weight(1f), elide = true)
        if (badge > 0) CountBadge(badge)
    }
}

@Composable
private fun GroupButton(group: GroupRow, selected: Boolean, nav: Nav) {
    SideRow(selected, { nav.openGroup(group.id) }, 40.dp) {
        Dot(group.online)
        Txt(group.title, Theme.d.fontMd, if (selected) Theme.c.textStrong else Theme.c.text, if (selected) 600 else 500, Modifier.weight(1f), elide = true)
    }
}

@Composable
private fun PeerButton(peer: PeerRow, selected: Boolean, nav: Nav) {
    SideRow(selected, { nav.openGroup(peer.groupId) }, 40.dp) {
        Dot(peer.connected)
        Txt(peer.title, Theme.d.fontMd, if (selected) Theme.c.textStrong else Theme.c.text, if (selected) 600 else 500, Modifier.weight(1f), elide = true)
    }
}

@Composable
private fun Sidebar(s: UiState, nav: Nav) {
    val backend = nav.backend
    val density = LocalDensity.current
    val handle = remember { MutableInteractionSource() }
    val handleHovered by handle.collectIsHoveredAsState()
    Row(Modifier.fillMaxHeight()) {
        Column(
            Modifier.width(backend.sidebarWidth.dp).fillMaxHeight().background(Theme.c.sidebarBg).padding(Theme.d.space3),
            verticalArrangement = Arrangement.spacedBy(Theme.d.space1),
        ) {
            Row(Modifier.padding(Theme.d.space1), horizontalArrangement = Arrangement.spacedBy(Theme.d.space3), verticalAlignment = Alignment.CenterVertically) {
                Logo(34.dp)
                Column(verticalArrangement = Arrangement.spacedBy(1.dp)) {
                    Txt("flipflop", Theme.d.fontLg, Theme.c.textStrong, 700)
                    NodeStatus(s)
                }
            }
            Box(Modifier.height(Theme.d.space3))
            SideButton("Home", Icons.inbox, nav.page == "home", s.home.size + s.invites.size + s.peers.count { it.pairRequest }) { nav.go("home") }
            Column(Modifier.weight(1f).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                val paired = s.peers.filter { it.groupId.isNotEmpty() }
                Row(Modifier.padding(start = Theme.d.space3, top = Theme.d.space2), verticalAlignment = Alignment.CenterVertically) {
                    Overline("PEERS", Modifier.weight(1f))
                }
                paired.forEach { PeerButton(it, nav.page == "group" && s.openGroup == it.groupId, nav) }
                if (paired.isEmpty()) {
                    Txt("No paired peers yet", Theme.d.fontSm, Theme.c.textFaint, modifier = Modifier.fillMaxWidth().height(40.dp), align = TextAlign.Center)
                }
                Row(Modifier.padding(start = Theme.d.space3, top = Theme.d.space2), verticalAlignment = Alignment.CenterVertically) {
                    Overline("GROUPS", Modifier.weight(1f))
                    IconButton(Icons.addPeer, tint = Theme.c.accentText, size = 28.dp) { nav.newGroup() }
                }
                s.invites.forEach { SideButton("Invitation: " + it.groupTitle, Icons.groups, false, 1) { nav.go("home") } }
                s.groups.forEach { GroupButton(it, nav.page == "group" && s.openGroup == it.id, nav) }
                if (s.groups.isEmpty() && s.invites.isEmpty()) {
                    Txt("No groups yet", Theme.d.fontSm, Theme.c.textFaint, modifier = Modifier.fillMaxWidth().height(40.dp), align = TextAlign.Center)
                }
            }
            SideButton("Peers", Icons.peers, nav.page == "peers") { nav.go("peers") }
            SideButton("Add a peer", Icons.addPeer, nav.page == "add") { nav.go("add") }
            SideButton("Guest link", Icons.link, false) { nav.newGuestLink() }
            SideButton("Settings", Icons.settings, nav.page == "settings") { nav.go("settings") }
        }
        // The divider doubles as a resize handle: a wide invisible strip around a hairline.
        Box(
            Modifier.width(7.dp).fillMaxHeight().hoverable(handle).pointerHoverIcon(horizontalResizeCursor())
                .draggable(
                    orientation = Orientation.Horizontal,
                    state = rememberDraggableState { delta ->
                        val step = with(density) { delta.toDp() }.value
                        backend.sidebarWidth = (backend.sidebarWidth + step).coerceIn(SIDEBAR_MIN_WIDTH.value, SIDEBAR_MAX_WIDTH.value)
                    },
                    onDragStopped = { backend.saveSidebarWidth() },
                ),
            contentAlignment = Alignment.Center,
        ) {
            Box(Modifier.width(1.dp).fillMaxHeight().background(if (handleHovered) Theme.c.borderStrong else Theme.c.border))
        }
    }
}

// ---- phone bottom navigation -----------------------------------------------------------------

@Composable
private fun RowScopeTab(modifier: Modifier, label: String, icon: DrawableResource, active: Boolean, badge: Int, onClick: () -> Unit) {
    val c = Theme.c
    Column(
        modifier.fillMaxHeight().pointerHoverIcon(PointerIcon.Hand).clickable(remember { MutableInteractionSource() }, null, onClick = onClick),
        verticalArrangement = Arrangement.spacedBy(4.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Box(Modifier.size(60.dp, 32.dp).clip(CircleShape).background(if (active) c.accentSoft else Color.Transparent), contentAlignment = Alignment.Center) {
            Icon(icon, 22.dp, if (active) c.accentText else c.textMuted)
            if (badge > 0) Box(Modifier.align(Alignment.TopCenter).padding(start = 28.dp)) { CountBadge(badge, 16) }
        }
        Txt(label, Theme.d.fontXs, if (active) c.textStrong else c.textMuted, if (active) 700 else 500, align = TextAlign.Center)
    }
}

@Composable
private fun BottomNav(s: UiState, nav: Nav) {
    Column(Modifier.fillMaxWidth().background(Theme.c.windowBg)) {
        Divider()
        Row(Modifier.fillMaxWidth().height(64.dp)) {
            RowScopeTab(Modifier.weight(1f), "Home", Icons.inbox, nav.page == "home", s.home.size + s.invites.size + s.peers.count { it.pairRequest }) {
                if (nav.page == "group") nav.backend.app.openGroup(null)
                nav.go("home")
            }
            RowScopeTab(Modifier.weight(1f), "Groups", Icons.groups, nav.page == "groups" || (nav.page == "group" && !s.groupImplied), 0) {
                if (nav.page == "group") nav.backend.app.openGroup(null)
                nav.go("groups")
            }
            RowScopeTab(Modifier.weight(1f), "Peers", Icons.peers, nav.page == "peers" || nav.page == "add" || (nav.page == "group" && s.groupImplied), 0) { nav.go("peers") }
            RowScopeTab(Modifier.weight(1f), "Settings", Icons.settings, nav.page == "settings", 0) { nav.go("settings") }
        }
    }
}

// ---- dialogs ---------------------------------------------------------------------------------

@Composable
private fun Scrim(onDismiss: () -> Unit, content: @Composable () -> Unit) {
    Box(
        Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.55f)).clickable(remember { MutableInteractionSource() }, null, onClick = onDismiss),
        contentAlignment = Alignment.TopCenter,
    ) { content() }
}

/** Files dragged over the window (desktop): dropping them sends them to the group showing, or asks where they go. */
@Composable
private fun DropHint(s: UiState, nav: Nav) {
    val c = Theme.c
    val d = Theme.d
    val group = if (nav.groupShowing()) {
        s.groups.firstOrNull { it.id == s.openGroup }?.title ?: s.peers.firstOrNull { it.groupId == s.openGroup }?.title
    } else null
    Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.55f)).padding(d.pagePadding), contentAlignment = Alignment.Center) {
        Column(
            Modifier.fillMaxSize().clip(RoundedCornerShape(d.radiusLg)).background(c.windowBg.copy(alpha = 0.92f))
                .border(2.dp, c.accent, RoundedCornerShape(d.radiusLg)),
            verticalArrangement = Arrangement.spacedBy(d.space3, Alignment.CenterVertically),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Icon(Icons.share, 40.dp, c.accent)
            if (group != null) {
                Txt("Drop to share with $group", d.fontXl, c.textStrong, 700, align = TextAlign.Center)
            } else {
                Txt("Drop to share", d.fontXl, c.textStrong, 700)
                Txt("Then pick a group or peer to send it to", d.fontSm, c.textMuted)
            }
        }
    }
}

/** Rename / delete a peer or a group. */
@Composable
private fun EditDialog(edit: Edit, nav: Nav) {
    val c = Theme.c
    val d = Theme.d
    val app = nav.backend.app
    val save = {
        app.saveEdit(edit.kind, edit.id, edit.text)
        nav.edit = null
    }
    Scrim({ nav.edit = null }) {
        Column(
            Modifier.windowInsetsPadding(WindowInsets.safeDrawing).padding(top = 80.dp, start = 16.dp, end = 16.dp).widthIn(max = 400.dp).fillMaxWidth()
                .clip(RoundedCornerShape(d.radiusLg)).background(c.cardBg).border(1.dp, c.borderStrong, RoundedCornerShape(d.radiusLg))
                .clickable(remember { MutableInteractionSource() }, null) {}.padding(d.cardPadding),
            verticalArrangement = Arrangement.spacedBy(d.space2),
        ) {
            Txt(if (edit.kind == "peer") "Rename peer" else "Rename group", d.fontLg, c.textStrong, 700)
            Txt(edit.original, d.fontSm, c.textMuted)
            Field(edit.text, { edit.text = it }, placeholder = "Local name (empty restores the original)", onAccepted = { save() })
            Row(horizontalArrangement = Arrangement.spacedBy(d.space2)) {
                PrimaryButton("Save") { save() }
                SecondaryButton("Cancel") { nav.edit = null }
                DangerButton(if (edit.kind == "peer") "Remove" else "Leave") {
                    app.deleteEdit(edit.kind, edit.id)
                    nav.edit = null
                    if (edit.kind == "group") nav.closeGroup()
                }
            }
        }
    }
}

/** Installed apps to share (Android). */
@Composable
private fun AppPicker(nav: Nav) {
    val c = Theme.c
    val d = Theme.d
    Scrim({ nav.appPickerOpen = false }) {
        Column(
            Modifier.fillMaxSize().padding(top = 80.dp).clip(RoundedCornerShape(topStart = d.radiusLg, topEnd = d.radiusLg)).background(c.windowBg)
                .clickable(remember { MutableInteractionSource() }, null) {}.windowInsetsPadding(WindowInsets.safeDrawing).padding(d.pagePadding),
            verticalArrangement = Arrangement.spacedBy(d.space3),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Txt("Share an app", d.fontXl, c.textStrong, 700, Modifier.weight(1f))
                IconButton(Icons.close) { nav.appPickerOpen = false }
            }
            Column(Modifier.weight(1f).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(d.space2)) {
                nav.apps.forEach { a ->
                    ClickCard({
                        nav.appPickerOpen = false
                        nav.backend.app.shareApp(a.path, a.name)
                    }) {
                        Column(Modifier.padding(d.space3), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                            Txt(a.name, d.fontMd, c.textStrong, 650, elide = true)
                            Txt(a.detail, d.fontXs, c.textMuted, elide = true)
                        }
                    }
                }
                if (nav.apps.isEmpty()) HintText("No apps found.", center = true)
            }
        }
    }
}

/** A group or a paired peer's page, as a place to share to. */
private class Target(val id: String, val title: String, val detail: String, val online: Boolean, val peer: Boolean)

/** Where to send what another app shared with us (Android) or was dropped on the window (desktop): online groups and peers first, fully offline ones last. */
@Composable
private fun SharePicker(s: UiState, share: Share, nav: Nav) {
    val c = Theme.c
    val d = Theme.d
    val peers = s.peers.filter { it.groupId.isNotEmpty() }.map { p: PeerRow ->
        Target(p.groupId, p.title, if (p.connected) "Paired peer · connected" else "Paired peer · offline", p.connected, true)
    }
    val groups = s.groups.map { g: GroupRow -> Target(g.id, g.title, g.summary, g.online, false) }
    // Stable: peers before groups within each half, each in its usual order.
    val targets = (peers + groups).sortedBy { !it.online }
    val what = listOfNotNull(
        if (share.text.isNotEmpty()) "a text" else null,
        when (share.files) { 0 -> null; 1 -> "a file"; else -> "${share.files} files" },
        when (share.folders) { 0 -> null; 1 -> "a folder"; else -> "${share.folders} folders" },
    ).joinToString(" and ")
    Scrim({ nav.backend.share = null }) {
        Column(
            Modifier.fillMaxSize().padding(top = 80.dp).clip(RoundedCornerShape(topStart = d.radiusLg, topEnd = d.radiusLg)).background(c.windowBg)
                .clickable(remember { MutableInteractionSource() }, null) {}.windowInsetsPadding(WindowInsets.safeDrawing).padding(d.pagePadding),
            verticalArrangement = Arrangement.spacedBy(d.space3),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Txt("Share $what", d.fontXl, c.textStrong, 700)
                    if (share.text.isNotEmpty()) Txt(share.text, d.fontSm, c.textMuted, elide = true)
                }
                IconButton(Icons.close) { nav.backend.share = null }
            }
            Column(Modifier.weight(1f).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(d.space2)) {
                targets.forEach { t ->
                    ClickCard({ nav.shareTo(t.id) }, border = if (t.peer) c.peerBorder else c.groupBorder, borderWidth = 2.dp) {
                        Row(Modifier.padding(d.space3), horizontalArrangement = Arrangement.spacedBy(d.space3), verticalAlignment = Alignment.CenterVertically) {
                            Dot(t.online, 10.dp, offline = c.error)
                            Icon(if (t.peer) Icons.person else Icons.groups, tint = c.textMuted)
                            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                                Txt(t.title, d.fontMd, c.textStrong, 650, elide = true)
                                Txt(t.detail, d.fontXs, c.textMuted, elide = true)
                            }
                        }
                    }
                }
                if (targets.isEmpty()) {
                    HintText(if (s.nodeReady) "No groups or paired peers yet. Create a group or pair with a peer first." else "Starting…", center = true)
                }
            }
        }
    }
}
