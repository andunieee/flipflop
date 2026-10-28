package dev.flipflop.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import uniffi.flipflop.FileRow
import uniffi.flipflop.MemberRow
import uniffi.flipflop.TabRow
import uniffi.flipflop.TextRow
import uniffi.flipflop.UiState
import uniffi.flipflop.mapCursor

@Composable
private fun FileCard(f: FileRow, nav: Nav) {
    val app = nav.backend.app
    val c = Theme.c
    val d = Theme.d
    Card {
        Column(Modifier.padding(d.cardPadding), verticalArrangement = Arrangement.spacedBy(d.space2)) {
            Row(horizontalArrangement = Arrangement.spacedBy(d.space3)) {
                IconTile(
                    if (f.kind == 1) Icons.image else if (f.kind == 2) Icons.apps else Icons.file,
                    tint = if (f.stopped) c.chipBg else c.accentSoft,
                    ink = if (f.stopped) c.textFaint else c.accentText,
                )
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Txt(f.name, d.fontMd, if (f.stopped) c.textMuted else c.textStrong, 650)
                    Txt(f.size + " · " + f.meta, d.fontSm, c.textMuted)
                    if (f.audience.isNotEmpty()) Txt(f.audience, d.fontSm, c.textMuted)
                    if (f.hosts.isNotEmpty()) Txt(f.hosts, d.fontSm, c.textMuted)
                }
            }
            if (f.downloading) {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    ProgressBar(f.progress)
                    Txt(f.progressLabel, d.fontXs, c.textFaint)
                }
            }
            if (f.error.isNotEmpty() && !f.downloading) Txt(f.error, d.fontSm, c.error)
            if (f.savedTo.isNotEmpty() && !f.mine) Txt("Saved to " + f.savedTo, d.fontXs, c.textFaint)
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(d.space2, Alignment.End), verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.weight(1f)) {
                    when {
                        f.stopped -> Badge("Not shared anymore", "muted")
                        f.mine -> Badge("Sharing", "active")
                        f.hosting -> Badge("Hosting", "active")
                        f.have -> Badge("Downloaded", "ok")
                        f.downloading -> Badge("Downloading", "active")
                        !f.canDownload -> Badge("Sender offline", "warn")
                    }
                }
                if (f.canDownload) PrimaryButton(if (f.error.isNotEmpty()) "Retry" else "Download", icon = Icons.arrowDown) { app.downloadFile(f.id) }
                if (f.savedTo.isNotEmpty()) SecondaryButton("Open", icon = Icons.open) { app.openFile(f.savedTo) }
                if (f.canHost) SecondaryButton("Host it too", icon = Icons.share) { app.hostFile(f.id, true) }
                if (f.hosting) SecondaryButton("Stop hosting") { app.hostFile(f.id, false) }
                if (f.mine && !f.stopped) DangerButton("Stop sharing") { app.stopFile(f.id) }
            }
        }
    }
}

@Composable
private fun FilesTab(s: UiState, nav: Nav) {
    val d = Theme.d
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(d.space3)) {
        Card {
            Column(Modifier.padding(d.cardPadding), verticalArrangement = Arrangement.spacedBy(d.space3)) {
                Row(horizontalArrangement = Arrangement.spacedBy(d.space2)) {
                    PrimaryButton("File", Modifier.weight(1f), Icons.file) { nav.pick("file") }
                    SecondaryButton("Media", Modifier.weight(1f), Icons.image) { nav.pick("media") }
                    if (nav.backend.touch) SecondaryButton("App", Modifier.weight(1f), Icons.apps) { nav.pick("app") }
                }
            }
        }
        s.files.forEach { FileCard(it, nav) }
        if (s.files.isEmpty()) HintText("Nothing offered yet.", center = true)
    }
}

@Composable
private fun TextCard(t: TextRow, nav: Nav) {
    val app = nav.backend.app
    val d = Theme.d
    Card {
        Column(Modifier.padding(d.cardPadding), verticalArrangement = Arrangement.spacedBy(d.space2)) {
            Row(horizontalArrangement = Arrangement.spacedBy(d.space2), verticalAlignment = Alignment.CenterVertically) {
                Txt(if (t.mine) t.name + " (you)" else t.name, d.fontSm, Theme.c.accentText, 650, Modifier.weight(1f), elide = true)
                Txt(t.time, d.fontXs, Theme.c.textFaint)
            }
            Txt(t.text, d.fontMd, Theme.c.textStrong)
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(d.space2, Alignment.End), verticalAlignment = Alignment.CenterVertically) {
                if (t.mine) IconButton(Icons.trash, danger = true) { app.deleteText(t.id) }
                if (t.isLink) SecondaryButton("Open", icon = Icons.open) { app.openLink(t.text) }
                SecondaryButton("Copy", icon = Icons.copy) { app.copyText(t.text) }
            }
        }
    }
}

@Composable
private fun TextsTab(s: UiState, nav: Nav) {
    val app = nav.backend.app
    val d = Theme.d
    var draft by remember { mutableStateOf("") }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(d.space3)) {
        Card {
            Column(Modifier.padding(d.cardPadding), verticalArrangement = Arrangement.spacedBy(d.space2)) {
                Area(draft, { draft = it }, placeholder = "A link, a code, anything to paste on the other devices…")
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(d.space2, Alignment.End)) {
                    SecondaryButton("Paste", icon = Icons.clipboard) { draft += app.pasteText() }
                    PrimaryButton("Send", icon = Icons.send, enabled = draft.isNotEmpty()) {
                        app.sendText(draft)
                        draft = ""
                    }
                }
            }
        }
        s.texts.forEach { TextCard(it, nav) }
        if (s.texts.isEmpty()) HintText("Nothing sent or received yet.", center = true)
    }
}

/**
 * The scratchpad: a big plain-text editor. Rust replaces the text when someone else edits; the cursor
 * is kept where it belongs (in characters, i.e. code points, which is what Rust counts).
 */
@Composable
private fun PadTab(s: UiState, nav: Nav) {
    val app = nav.backend.app
    val c = Theme.c
    val d = Theme.d
    var value by remember(s.openGroup) { mutableStateOf(TextFieldValue(s.padText)) }
    var focused by remember { mutableStateOf(false) }
    LaunchedEffect(s.padText, s.padLocal) {
        if (!s.padLocal && s.padText != value.text) {
            val old = value.text
            val new = s.padText
            fun map(i: Int): Int {
                val chars = old.codePointCount(0, i.coerceIn(0, old.length))
                return new.offsetByCodePoints(0, mapCursor(old, new, chars.toUInt()).toInt().coerceAtMost(new.codePointCount(0, new.length)))
            }
            value = TextFieldValue(new, if (focused) TextRange(map(value.selection.start), map(value.selection.end)) else TextRange(new.length))
        }
    }
    val edit = { new: TextFieldValue ->
        val changed = new.text != value.text
        value = new
        if (changed) app.padEdited(new.text)
    }
    val primary = rememberPrimarySelection(value, edit)
    Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.spacedBy(d.space2)) {
        val shape = RoundedCornerShape(d.radiusLg)
        Box(
            Modifier.weight(1f).fillMaxWidth().clip(shape).background(c.cardBg)
                .border(1.dp, if (focused) c.accent else c.border, shape)
                .verticalScroll(rememberScrollState()).padding(d.cardPadding),
        ) {
            BasicTextField(
                value = value,
                onValueChange = edit,
                onTextLayout = { primary.layout = it },
                textStyle = TextStyle(color = c.textStrong, fontSize = d.fontMd),
                cursorBrush = SolidColor(c.accent),
                modifier = Modifier.fillMaxWidth().heightIn(min = 200.dp).then(primary.modifier).onFocus { focused = it },
            )
            if (value.text.isEmpty() && !focused) {
                Txt("Everyone in the group can edit this field.", d.fontMd, c.textFaint)
            }
        }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(d.space2, Alignment.End)) {
            SecondaryButton("Copy all", icon = Icons.copy) { app.copyText(value.text) }
            SecondaryButton("Paste", icon = Icons.clipboard) {
                val pasted = app.pasteText()
                if (pasted.isNotEmpty()) {
                    val at = if (focused) value.selection.min else value.text.length
                    val text = value.text.substring(0, at) + pasted + value.text.substring(at)
                    edit(TextFieldValue(text, TextRange(at + pasted.length)))
                }
            }
        }
    }
}

/** An app's tab. Running apps and showing their interface comes next; for now, where its file is. */
@Composable
private fun AppTab(t: TabRow) {
    val d = Theme.d
    Card {
        Column(
            Modifier.padding(d.space5).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(d.space3),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            IconTile(Icons.apps, 56.dp)
            Txt(t.title, d.fontLg, Theme.c.textStrong, 700, align = TextAlign.Center)
            Txt(t.detail, d.fontSm, Theme.c.textMuted, align = TextAlign.Center)
            if (t.status.isNotEmpty()) Badge(t.status, t.tone)
            Txt(
                "Every device in the group fetches this app and shares it with the others. Running it here, with its hypereffect.dev interface, is coming.",
                d.fontMd, Theme.c.textMuted, align = TextAlign.Center,
            )
        }
    }
}

/** The group's tabs, the same for every member: anyone shows or hides them, or adds an app. */
@Composable
private fun TabsCard(s: UiState, nav: Nav) {
    val app = nav.backend.app
    val d = Theme.d
    Overline("TABS")
    Card {
        Column(Modifier.padding(d.cardPadding), verticalArrangement = Arrangement.spacedBy(d.space2)) {
            s.tabs.forEachIndexed { i, t ->
                if (i > 0) Divider()
                val hint = when (t.id) {
                    "files" -> "Files offered to the group."
                    "texts" -> "Links, codes and snippets sent to the group."
                    "pad" -> "A text everyone edits together."
                    else -> t.detail
                }
                ToggleRow(t.title, hint, t.on) { app.setTab(t.id, it) }
            }
            Divider()
            SecondaryButton("Add an app", icon = Icons.apps) { nav.pick("wasm") }
        }
    }
    HintText("Tabs are the same for everyone in the group, and anyone can show or hide them. An app is a WebAssembly file: every device fetches it and shares it with the others.")
}

@Composable
private fun MemberLine(m: MemberRow) {
    Row(Modifier.fillMaxWidth().heightIn(min = Theme.d.controlHeight + 12.dp), horizontalArrangement = Arrangement.spacedBy(Theme.d.space2), verticalAlignment = Alignment.CenterVertically) {
        Dot(m.online || m.me)
        Txt(if (m.me) m.name + " (you)" else m.name, Theme.d.fontMd, Theme.c.textStrong, modifier = Modifier.weight(1f), elide = true)
        Badge(m.status, m.tone)
    }
}

/** One address of a guest link: its QR code, the text and a Copy button. */
@Composable
private fun GuestAddress(title: String, hint: String, unavailable: String, url: String, nav: Nav) {
    val d = Theme.d
    Card {
        Column(Modifier.padding(d.cardPadding), verticalArrangement = Arrangement.spacedBy(d.space2)) {
            Txt(title, d.fontMd, Theme.c.textStrong, 650)
            if (url.isEmpty()) {
                HintText(unavailable)
            } else {
                HintText(hint)
                BoxWithConstraints(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                    QrCode(url, Modifier.size(minOf(maxWidth, 220.dp)))
                }
                Field(url, {}, readOnly = true)
                SecondaryButton("Copy link", icon = Icons.copy) { nav.backend.app.copyText(url) }
            }
        }
    }
}

@Composable
private fun GuestLinkCards(s: UiState, nav: Nav) {
    val app = nav.backend.app
    Overline("WEB GUESTS")
    Card {
        Column(Modifier.padding(Theme.d.cardPadding), verticalArrangement = Arrangement.spacedBy(Theme.d.space2)) {
            ToggleRow("Allow guests from a browser", "Anyone with the secret link joins this group from a web page, no app needed.", s.groupGuest) {
                if (it) app.enableGuestLink(false) else app.disableGuestLink()
            }
            if (s.groupGuest) {
                Divider()
                ToggleRow("Guests can upload files", "Even when off they can still download files, write texts and edit the scratchpad.", s.guestUploads) {
                    app.setGuestUploads(it)
                }
            }
        }
    }
    if (s.groupGuest) {
        GuestAddress("Same network", "Opens a page served by this device. Works without internet, as long as the guest is on the same Wi-Fi.", "Not available because you're not connected to any Wi-Fi networks.", s.guestLanUrl, nav)
        GuestAddress("Anywhere", "Opens the flipflop page on the web, uses Iroh to connect to this device.", "Not available because you're not connected to the internet.", s.guestWebUrl, nav)
        HintText("Anyone with a link joins the group and can do things in it. Only share it with people you trust.")
    }
}

@Composable
private fun InfoTab(s: UiState, nav: Nav) {
    val app = nav.backend.app
    val c = Theme.c
    val d = Theme.d
    var confirmLeave by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(d.space3)) {
        val peer = s.groupPeer
        if (s.groupImplied && peer != null) {
            Overline("PEER")
            Card {
                Column(Modifier.padding(d.cardPadding), verticalArrangement = Arrangement.spacedBy(d.space3)) {
                    Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                        Txt(peer.title, d.fontLg, c.textStrong, 650, elide = true)
                        Txt((if (peer.subtitle.isNotEmpty()) peer.subtitle + " · " else "") + peer.short, d.fontXs, c.textFaint, elide = true)
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Badge("paired", "ok")
                            if (peer.connected) Badge("connected", "ok") else Badge("offline", "muted")
                            if (peer.lan) Badge("LAN", "active")
                            if (peer.ble) Badge("Bluetooth", "warn")
                        }
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(d.space2)) {
                        SecondaryButton("Rename", icon = Icons.edit) {
                            nav.edit = Edit("peer", peer.id, peer.subtitle.ifEmpty { peer.title }, peer.alias)
                        }
                        GhostButton("Unpair") { app.unpairPeer(peer.id) }
                    }
                }
            }
            Overline("INVITE TO A GROUP")
            Card {
                Column(Modifier.padding(d.cardPadding), verticalArrangement = Arrangement.spacedBy(d.space2)) {
                    if (s.invitable.isNotEmpty()) {
                        Picker(s.invitable, nav.inviteChoice) { nav.inviteChoice = it }
                        PrimaryButton("Invite", icon = Icons.addPeer) { app.invitePeerTo(nav.inviteChoice) }
                    } else {
                        HintText("No group to invite them to: create a group first, or they are in all of yours already.")
                    }
                }
            }
        } else {
            Overline(if (s.groupGuest) "GUESTS AND MEMBERS" else "MEMBERS")
            Card {
                Column(Modifier.padding(d.cardPadding)) {
                    s.members.forEach { MemberLine(it) }
                    if (s.members.isEmpty()) HintText("Nobody yet.")
                    Divider()
                    Txt("Invite", d.fontSm, c.textMuted, modifier = Modifier.padding(top = d.space2))
                    s.candidates.forEach { cand ->
                        Row(Modifier.fillMaxWidth().heightIn(min = d.controlHeight + 12.dp), horizontalArrangement = Arrangement.spacedBy(d.space3), verticalAlignment = Alignment.CenterVertically) {
                            Txt(cand.name, d.fontMd, c.textStrong, modifier = Modifier.weight(1f), elide = true)
                            SecondaryButton("Invite") { app.addToGroup(s.openGroup, cand.id) }
                        }
                    }
                    if (s.candidates.isEmpty()) HintText("Every peer you know is already in, or has been invited.")
                }
            }
        }

        TabsCard(s, nav)

        Overline("ON THIS DEVICE")
        Card {
            Column(Modifier.padding(d.cardPadding), verticalArrangement = Arrangement.spacedBy(d.space2)) {
                ToggleRow("Download every file automatically", "New offers from the group are fetched as soon as the sender is reachable.", s.autoDownload) { app.setAutoDownload(it) }
                Divider()
                ToggleRow("Stop sharing a file once everybody has it", "Applies to the files you offer. You can always stop one by hand.", s.autoStop) { app.setAutoStop(it) }
                Divider()
                ToggleRow("Automatically rehost received files", "Serve the files you download to the rest of the group too, so they stay available when the sender is offline.", s.autoRehost) { app.setAutoRehost(it) }
            }
        }

        if (!s.groupImplied) GuestLinkCards(s, nav)

        if (!s.groupImplied) {
            Row(horizontalArrangement = Arrangement.spacedBy(d.space2)) {
                SecondaryButton("Rename", icon = Icons.edit) { nav.edit = Edit("group", s.openGroup, s.groupHaiku, s.groupAlias) }
                // Leaving needs a second press.
                val closing = if (s.groupGuest) "Close link" else "Leave group"
                DangerButton(if (confirmLeave) "Confirm" else closing, icon = Icons.trash) {
                    if (confirmLeave) {
                        confirmLeave = false
                        if (s.groupGuest) {
                            app.disableGuestLink()
                        } else {
                            app.deleteEdit("group", s.openGroup)
                            nav.closeGroup()
                        }
                    } else {
                        confirmLeave = true
                    }
                }
            }
            HintText(
                if (s.groupGuest) "Closing the link disconnects its guests and keeps the group, with its members and files."
                else "Leaving removes the group from this device only. Downloaded files are not affected.",
            )
        }
    }
}

/** A simple drop-down. */
@Composable
private fun Picker(options: List<String>, selected: Int, onPicked: (Int) -> Unit) {
    var open by remember { mutableStateOf(false) }
    val c = Theme.c
    val d = Theme.d
    val shape = RoundedCornerShape(d.radiusSm + 2.dp)
    Box {
        Row(
            Modifier.fillMaxWidth().heightIn(min = d.controlHeight).clip(shape).background(c.insetBg).border(1.dp, c.borderStrong, shape)
                .pointerHoverIcon(PointerIcon.Hand).clickable(remember { androidx.compose.foundation.interaction.MutableInteractionSource() }, null) { open = true }
                .padding(horizontal = d.space3),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Txt(options.getOrElse(selected) { "" }, d.fontMd, c.textStrong, modifier = Modifier.weight(1f), elide = true)
            Icon(Icons.chevronDown, tint = c.textMuted)
        }
        androidx.compose.material3.DropdownMenu(open, { open = false }) {
            options.forEachIndexed { i, o ->
                androidx.compose.material3.DropdownMenuItem(text = { Txt(o, d.fontMd, c.textStrong) }, onClick = { onPicked(i); open = false })
            }
        }
    }
}

/** One open group: header, tabs, and the tab's content. */
@Composable
fun GroupPage(s: UiState, nav: Nav) {
    val d = Theme.d
    val shown = s.tabs.filter { it.on }
    // The tab asked for may be hidden (by anyone in the group): show the first one there is.
    val tab = if (nav.groupTab == "info" || shown.any { it.id == nav.groupTab }) nav.groupTab else shown.firstOrNull()?.id ?: "info"
    Column(
        Modifier.fillMaxSize().padding(start = d.pagePadding, end = d.pagePadding, top = d.pagePadding, bottom = d.space3),
        verticalArrangement = Arrangement.spacedBy(d.space3),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(d.space2), verticalAlignment = Alignment.CenterVertically) {
            if (nav.backend.touch) IconButton(Icons.arrowLeft, tint = Theme.c.textStrong) { nav.closeGroup() }
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Txt(s.groupTitle, d.fontXl, Theme.c.textStrong, 700, elide = true)
                if (s.groupAlias.isNotEmpty()) Txt(s.groupHaiku, d.fontSm, Theme.c.textMuted, elide = true)
            }
            IconButton(Icons.settings, tint = if (nav.groupTab == "info") Theme.c.accentText else Theme.c.textMuted) {
                nav.groupTab = if (nav.groupTab == "info") shown.firstOrNull()?.id ?: "info" else "info"
            }
        }
        if (shown.isNotEmpty()) TabStrip(shown.map { it.title }, shown.map { it.id }, tab) { nav.groupTab = it }
        Box(Modifier.weight(1f).fillMaxWidth()) {
            when (tab) {
                "files" -> FilesTab(s, nav)
                "texts" -> TextsTab(s, nav)
                "pad" -> PadTab(s, nav)
                "info" -> InfoTab(s, nav)
                else -> shown.firstOrNull { it.id == tab }?.let { AppTab(it) }
            }
        }
    }
}
