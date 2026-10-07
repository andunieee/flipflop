package dev.flipflop.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import uniffi.flipflop.GroupRow
import uniffi.flipflop.HomeRow
import uniffi.flipflop.InviteRow
import uniffi.flipflop.PeerRow
import uniffi.flipflop.UiState
import uniffi.flipflop.qrMatrix

/** A page: padded, scrolling. */
@Composable
fun ScrollPage(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Column(
        modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(Theme.d.pagePadding),
        verticalArrangement = Arrangement.spacedBy(Theme.d.space3),
    ) { content() }
}

/** A page whose list scrolls under a fixed header. */
@Composable
fun ListPage(header: @Composable () -> Unit, content: @Composable () -> Unit) {
    val d = Theme.d
    Column(Modifier.fillMaxSize().padding(start = d.pagePadding, end = d.pagePadding, top = d.pagePadding, bottom = d.space3), verticalArrangement = Arrangement.spacedBy(d.space3)) {
        header()
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(d.space3)) { content() }
    }
}

@Composable
fun HeaderRow(title: String, subtitle: String, action: @Composable () -> Unit) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(Theme.d.space3)) {
        PageTitle(title, subtitle, Modifier.weight(1f))
        action()
    }
}

// ---- Home ------------------------------------------------------------------------------------

@Composable
private fun InviteCard(invite: InviteRow, nav: Nav) {
    val c = Theme.c
    Card(background = c.accentSoft, border = c.accentText.copy(alpha = 0.4f)) {
        Column(Modifier.padding(Theme.d.cardPadding), verticalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
            Txt(invite.inviterName + " invites you to join", Theme.d.fontSm, c.textMuted, elide = true)
            Txt(invite.groupTitle, Theme.d.fontLg, c.textStrong, 700)
            Row(horizontalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
                PrimaryButton("Accept") { nav.backend.app.acceptInvite(invite.inviterId, invite.groupId) }
                SecondaryButton("Decline") { nav.backend.app.rejectInvite(invite.inviterId, invite.groupId) }
            }
        }
    }
}

@Composable
private fun HomeCard(item: HomeRow, nav: Nav) {
    ClickCard({ nav.openGroup(item.groupId, item.tab) }) {
        Row(Modifier.padding(Theme.d.cardPadding), horizontalArrangement = Arrangement.spacedBy(Theme.d.space3), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Txt(item.title, Theme.d.fontLg, Theme.c.textStrong, 650)
                Txt(item.lines, Theme.d.fontSm, Theme.c.text)
            }
            Icon(Icons.chevronRight, tint = Theme.c.textFaint)
        }
    }
}

/** A paired peer that is online: its summary, opening its page. */
@Composable
private fun OnlinePeerCard(peer: PeerRow, nav: Nav) {
    ClickCard({ nav.openGroup(peer.groupId) }, border = Theme.c.peerBorder, borderWidth = 2.dp) {
        Row(Modifier.padding(Theme.d.cardPadding), horizontalArrangement = Arrangement.spacedBy(Theme.d.space3), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.person, tint = Theme.c.textMuted)
            PeerSummary(peer, Modifier.weight(1f), onHome = true)
            Icon(Icons.chevronRight, tint = Theme.c.textFaint)
        }
    }
}

@Composable
fun HomePage(s: UiState, nav: Nav) {
    ListPage(header = {
        HeaderRow("Home", "News and online peers.") {
            if (s.home.isNotEmpty()) SecondaryButton("Mark all as seen") { nav.backend.app.markAllSeen() }
        }
    }) {
        s.invites.forEach { InviteCard(it, nav) }
        if (s.home.isNotEmpty()) Overline("NEW")
        s.home.forEach { HomeCard(it, nav) }
        val peers = s.peers.filter { it.connected && it.groupId.isNotEmpty() }
        val groups = s.groups.filter { it.online }
        if (peers.isNotEmpty() || groups.isNotEmpty()) Overline("ONLINE NOW")
        peers.forEach { OnlinePeerCard(it, nav) }
        groups.forEach { GroupCard(it, nav, onHome = true) }
        if (s.home.isEmpty() && s.invites.isEmpty() && peers.isEmpty() && groups.isEmpty()) {
            HintText("All caught up. New files, texts and members of your groups show up here.", center = true)
        }
    }
}

// ---- Groups ----------------------------------------------------------------------------------

/** [onHome]: listed under "online now", so the online dot goes without saying. */
@Composable
private fun GroupCard(group: GroupRow, nav: Nav, onHome: Boolean = false) {
    ClickCard({ nav.openGroup(group.id) }, border = Theme.c.groupBorder, borderWidth = 2.dp) {
        Row(Modifier.padding(Theme.d.cardPadding), horizontalArrangement = Arrangement.spacedBy(Theme.d.space3), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.groups, tint = Theme.c.textMuted)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(Theme.d.space2), verticalAlignment = Alignment.CenterVertically) {
                    if (!onHome) Dot(group.online)
                    Txt(group.title, Theme.d.fontLg, Theme.c.textStrong, 650, Modifier.weight(1f))
                    if (group.guest) Badge("guest link", "active")
                }
                if (group.subtitle.isNotEmpty()) Txt(group.subtitle, Theme.d.fontSm, Theme.c.textMuted)
                Txt(group.summary, Theme.d.fontSm, Theme.c.textMuted)
            }
            Icon(Icons.chevronRight, tint = Theme.c.textFaint)
        }
    }
}

@Composable
fun GroupsPage(s: UiState, nav: Nav) {
    ListPage(header = {
        HeaderRow("Groups", "") {
            Row(horizontalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
                SecondaryButton("Guest link", icon = Icons.link) { nav.newGuestLink() }
                PrimaryButton("New group", icon = Icons.addPeer) { nav.newGroup() }
            }
        }
    }) {
        s.invites.forEach { InviteCard(it, nav) }
        s.groups.forEach { GroupCard(it, nav) }
        if (s.groups.isEmpty() && s.invites.isEmpty()) {
            HintText("No groups yet.", center = true)
        }
    }
}

// ---- Peers -----------------------------------------------------------------------------------

/** [onHome]: listed under "online now", so "paired" and "connected" go without saying. */
@Composable
fun PeerBadges(peer: PeerRow, onHome: Boolean = false) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        if (peer.groupId.isNotEmpty() && !onHome) Badge("paired", "ok")
        if (peer.groupId.isEmpty() && peer.pairedUs && !peer.paired) Badge("wants to pair", "warn")
        if (peer.groupId.isEmpty() && peer.paired && !peer.pairedUs) Badge("waiting for them", "muted")
        if (peer.connected && !onHome) Badge("connected", "ok")
        if (peer.saved && !peer.connected) Badge("offline", "muted")
        if (peer.lan) Badge("LAN", "active")
        if (peer.ble) Badge("Bluetooth", "warn")
    }
}

/** A peer's name, own name and key prefix, and badges. */
@Composable
private fun PeerSummary(peer: PeerRow, modifier: Modifier = Modifier, onHome: Boolean = false) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(3.dp)) {
        Txt(peer.title, Theme.d.fontLg, Theme.c.textStrong, 650, elide = true)
        Txt((if (peer.subtitle.isNotEmpty()) peer.subtitle + " · " else "") + peer.short, Theme.d.fontXs, Theme.c.textFaint, elide = true)
        PeerBadges(peer, onHome)
    }
}

@Composable
private fun PeerCard(peer: PeerRow, nav: Nav) {
    val app = nav.backend.app
    val body: @Composable () -> Unit = {
        Row(Modifier.padding(Theme.d.cardPadding), horizontalArrangement = Arrangement.spacedBy(Theme.d.space3)) {
            Icon(Icons.person, tint = Theme.c.textMuted, modifier = Modifier.align(Alignment.CenterVertically))
            PeerSummary(peer, Modifier.weight(1f).align(Alignment.CenterVertically))
            Column(Modifier.align(Alignment.CenterVertically), verticalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
                if (!peer.paired) PrimaryButton("Pair", icon = Icons.addPeer) { app.pairPeer(peer.id) }
                if (peer.paired && peer.groupId.isEmpty()) GhostButton("Unpair") { app.unpairPeer(peer.id) }
                if (!peer.connected) SecondaryButton("Connect") { app.connect(peer.id) }
                if (peer.saved) GhostButton("Edit", icon = Icons.edit) {
                    nav.edit = Edit("peer", peer.id, peer.subtitle.ifEmpty { peer.title }, peer.alias)
                }
            }
        }
    }
    if (peer.groupId.isNotEmpty()) ClickCard({ nav.openGroup(peer.groupId) }, border = Theme.c.peerBorder, borderWidth = 2.dp) { body() }
    else Card(border = Theme.c.peerBorder, borderWidth = 2.dp) { body() }
}

@Composable
fun PeersPage(s: UiState, nav: Nav) {
    ListPage(header = {
        HeaderRow("Peers", "Devices nearby and the ones you saved. Pair with a peer, and when it pairs back you get a page of your own with it.") {
            PrimaryButton("Add", icon = Icons.addPeer) { nav.go("add") }
        }
    }) {
        s.peers.forEach { PeerCard(it, nav) }
        if (s.peers.isEmpty()) HintText("No one around yet. Other flipflop devices on this network or in Bluetooth range show up here.", center = true)
    }
}

// ---- Add a peer ------------------------------------------------------------------------------

@Composable
fun QrCode(ticket: String, modifier: Modifier = Modifier) {
    val matrix = remember(ticket) { if (ticket.isEmpty()) null else qrMatrix(ticket) }
    Box(modifier.clip(RoundedCornerShape(Theme.d.radius)).background(Color.White).padding(10.dp), contentAlignment = Alignment.Center) {
        if (matrix != null) {
            androidx.compose.foundation.Canvas(Modifier.fillMaxSize()) {
                val n = matrix.size.toInt()
                val cell = size.width / n
                for (y in 0 until n) for (x in 0 until n) {
                    if (matrix.dark[y * n + x]) {
                        drawRect(
                            Color.Black,
                            topLeft = androidx.compose.ui.geometry.Offset(x * cell, y * cell),
                            size = androidx.compose.ui.geometry.Size(cell + 0.5f, cell + 0.5f),
                        )
                    }
                }
            }
        }
    }
}

@Composable
fun AddPage(s: UiState, nav: Nav) {
    val app = nav.backend.app
    var pasted by remember { mutableStateOf("") }
    ScrollPage {
        PageTitle("Add a peer", "Scan a code from the other device, or show yours.")
        Card {
            Column(Modifier.padding(Theme.d.cardPadding), verticalArrangement = Arrangement.spacedBy(Theme.d.space3)) {
                Txt("Your code", Theme.d.fontMd, Theme.c.textStrong, 650)
                BoxWithConstraints(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                    QrCode(s.ticket, Modifier.size(minOf(maxWidth, 260.dp)))
                }
                PrimaryButton("Scan a code", icon = Icons.qr) { app.scan() }
            }
        }
        Card {
            Column(Modifier.padding(Theme.d.cardPadding), verticalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
                Txt("…or paste a ticket", Theme.d.fontMd, Theme.c.textStrong, 650)
                Field(pasted, { pasted = it }, placeholder = "paste the other device's ticket", onAccepted = { app.connectTicket(it) })
                SecondaryButton("Connect", enabled = pasted.isNotEmpty()) { app.connectTicket(pasted) }
            }
        }
        Card {
            Column(Modifier.padding(Theme.d.cardPadding), verticalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
                Txt("Your ticket", Theme.d.fontMd, Theme.c.textStrong, 650)
                Field(s.ticket, {}, readOnly = true)
                SecondaryButton("Copy", icon = Icons.copy) { app.copyText(s.ticket) }
            }
        }
    }
}

/** The webcam preview while scanning (desktop). */
@Composable
fun ScanView(nav: Nav) {
    val frame by nav.backend.camera.collectAsState()
    Column(Modifier.fillMaxSize().padding(Theme.d.pagePadding), verticalArrangement = Arrangement.spacedBy(Theme.d.space3)) {
        Txt("Point the camera at a flipflop code", Theme.d.fontMd, Theme.c.textStrong, modifier = Modifier.fillMaxWidth(), align = TextAlign.Center)
        Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
            frame?.let { Image(it, null, Modifier.fillMaxSize(), contentScale = ContentScale.Fit) }
        }
        SecondaryButton("Cancel") { nav.backend.app.cancelScan() }
    }
}

// ---- Settings --------------------------------------------------------------------------------

@Composable
fun SettingsPage(s: UiState, nav: Nav) {
    val app = nav.backend.app
    var name by remember(s.myName) { mutableStateOf(s.myName) }
    ScrollPage {
        PageTitle("Settings")
        Card {
            Column(Modifier.padding(Theme.d.cardPadding), verticalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
                Txt("This device's name", Theme.d.fontMd, Theme.c.textStrong, 650)
                HintText("Shown to other members next to the texts you send.")
                Field(name, { name = it }, placeholder = "Device name", onAccepted = { app.rename(it) })
                PrimaryButton("Save") { app.rename(name) }
            }
        }
        Card {
            Column(Modifier.padding(Theme.d.cardPadding), verticalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
                Txt("Appearance", Theme.d.fontMd, Theme.c.textStrong, 650)
                TabStrip(listOf("System", "Light", "Dark"), listOf("system", "light", "dark"), nav.backend.theme) { nav.backend.theme = it }
            }
        }
        Card {
            Column(Modifier.padding(Theme.d.cardPadding), verticalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
                Txt("Downloads", Theme.d.fontMd, Theme.c.textStrong, 650)
                Txt(s.downloadDir, Theme.d.fontSm, Theme.c.textMuted)
            }
        }
        Card {
            Column(Modifier.padding(Theme.d.cardPadding), verticalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
                Txt("Connection", Theme.d.fontMd, Theme.c.textStrong, 650)
                Txt(s.bleStatus.ifEmpty { "Starting…" }, Theme.d.fontSm, Theme.c.textMuted)
            }
        }
    }
}
