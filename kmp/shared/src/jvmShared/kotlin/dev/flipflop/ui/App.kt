package dev.flipflop.ui

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
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
import uniffi.flipflop.UiState

/** The whole app, over a running [Backend]. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun FlipflopApp(backend: Backend) {
    val state by backend.state.collectAsState()
    val s = state
    BoxWithConstraints(Modifier.fillMaxSize()) {
        FlipflopTheme(touch = backend.touch, narrow = maxWidth < 420.dp) {
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
            .hoverable(source).clickable(source, indication = null, onClick = onClick).padding(horizontal = Theme.d.space3),
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
private fun Sidebar(s: UiState, nav: Nav) {
    Row(Modifier.width(241.dp).fillMaxHeight()) {
        Column(
            Modifier.width(240.dp).fillMaxHeight().background(Theme.c.sidebarBg).padding(Theme.d.space3),
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
            SideButton("Home", Icons.inbox, nav.page == "home", s.home.size + s.invites.size) { nav.go("home") }
            Row(Modifier.padding(start = Theme.d.space3), verticalAlignment = Alignment.CenterVertically) {
                Overline("GROUPS", Modifier.weight(1f))
                IconButton(Icons.addPeer, tint = Theme.c.accentText, size = 28.dp) { nav.newGroup() }
            }
            Column(Modifier.weight(1f).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(2.dp)) {
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
        Box(Modifier.width(1.dp).fillMaxHeight().background(Theme.c.border))
    }
}

// ---- phone bottom navigation -----------------------------------------------------------------

@Composable
private fun RowScopeTab(modifier: Modifier, label: String, icon: DrawableResource, active: Boolean, badge: Int, onClick: () -> Unit) {
    val c = Theme.c
    Column(
        modifier.fillMaxHeight().clickable(remember { MutableInteractionSource() }, null, onClick = onClick),
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
            RowScopeTab(Modifier.weight(1f), "Home", Icons.inbox, nav.page == "home", s.home.size + s.invites.size) {
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
