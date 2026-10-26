package dev.flipflop.ui

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.hoverable
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.jetbrains.compose.resources.DrawableResource
import org.jetbrains.compose.resources.painterResource

/** Text with the app's defaults. */
@Composable
fun Txt(
    text: String,
    size: TextUnit = Theme.d.fontMd,
    color: Color = Theme.c.text,
    weight: Int = 400,
    modifier: Modifier = Modifier,
    align: TextAlign? = null,
    maxLines: Int = Int.MAX_VALUE,
    elide: Boolean = false,
    spacing: TextUnit = TextUnit.Unspecified,
) {
    Text(
        text = text,
        modifier = modifier,
        fontSize = size,
        color = color,
        fontWeight = FontWeight(weight),
        textAlign = align,
        maxLines = if (elide) 1 else maxLines,
        overflow = if (elide) TextOverflow.Ellipsis else TextOverflow.Clip,
        letterSpacing = spacing,
    )
}

@Composable
fun Icon(res: DrawableResource, size: Dp = 18.dp, tint: Color = Theme.c.text, modifier: Modifier = Modifier) {
    Icon(painterResource(res), contentDescription = null, tint = tint, modifier = modifier.size(size))
}

@Composable
fun PageTitle(title: String, subtitle: String = "", modifier: Modifier = Modifier) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(Theme.d.space1)) {
        Txt(title, Theme.d.fontXl, Theme.c.textStrong, 700, elide = true)
        if (subtitle.isNotEmpty()) Txt(subtitle, Theme.d.fontSm, Theme.c.textMuted)
    }
}

/** Small all-caps label above a list. */
@Composable
fun Overline(label: String, modifier: Modifier = Modifier) {
    Txt(label, Theme.d.fontXs, Theme.c.textFaint, 650, modifier, spacing = 0.6.sp)
}

@Composable
fun HintText(label: String, modifier: Modifier = Modifier, center: Boolean = false) {
    Txt(label, Theme.d.fontSm, Theme.c.textMuted, modifier = modifier.fillMaxWidth(), align = if (center) TextAlign.Center else null)
}

@Composable
fun Card(modifier: Modifier = Modifier, background: Color = Theme.c.cardBg, border: Color = Theme.c.border, onClick: (() -> Unit)? = null, content: @Composable ColumnScope.() -> Unit) {
    val shape = RoundedCornerShape(Theme.d.radiusLg)
    var m = modifier.fillMaxWidth().clip(shape).background(background).border(1.dp, border, shape)
    if (onClick != null) m = m.clickable(onClick = onClick)
    Column(m, content = content)
}

/** A card that darkens a little under the pointer. */
@Composable
fun ClickCard(onClick: () -> Unit, modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    val hover by source.collectIsHoveredAsState()
    val c = Theme.c
    val shape = RoundedCornerShape(Theme.d.radiusLg)
    Column(
        modifier.fillMaxWidth().clip(shape)
            .background(if (pressed) c.chipBg else if (hover) c.insetBg else c.cardBg)
            .border(1.dp, c.border, shape)
            .hoverable(source).clickable(source, indication = null, onClick = onClick),
        content = content,
    )
}

@Composable
fun Divider() {
    Box(Modifier.fillMaxWidth().height(1.dp).background(Theme.c.border))
}

@Composable
fun Dot(online: Boolean, size: Dp = 8.dp) {
    Box(Modifier.size(size).clip(CircleShape).background(if (online) Theme.c.online else Theme.c.offline))
}

/** Pill with a tinted background. tone: "ok" | "error" | "warn" | "muted" | "active" */
@Composable
fun Badge(label: String, tone: String = "muted") {
    val c = Theme.c
    val (bg, fg) = when (tone) {
        "ok" -> c.successSoft to c.success
        "error" -> c.errorSoft to c.error
        "warn" -> c.warnSoft to c.warn
        "active" -> c.infoSoft to c.info
        else -> c.chipBg to c.textMuted
    }
    Box(Modifier.clip(CircleShape).background(bg).padding(horizontal = 8.dp, vertical = 2.dp)) {
        Txt(label, Theme.d.fontXs, fg, 600, maxLines = 1)
    }
}

/** Round tile with an icon (file kinds, section headers). */
@Composable
fun IconTile(icon: DrawableResource, size: Dp = 40.dp, tint: Color = Theme.c.accentSoft, ink: Color = Theme.c.accentText) {
    Box(Modifier.size(size).clip(RoundedCornerShape(size * 0.3f)).background(tint), contentAlignment = Alignment.Center) {
        Icon(icon, size * 0.5f, ink)
    }
}

@Composable
fun ProgressBar(value: Float, modifier: Modifier = Modifier, tint: Color = Theme.c.accent) {
    Box(modifier.fillMaxWidth().height(6.dp).clip(CircleShape).background(Theme.c.insetBg)) {
        Box(Modifier.fillMaxHeight().fillMaxWidth(value.coerceIn(0f, 1f)).clip(CircleShape).background(tint))
    }
}

enum class ButtonKind { Primary, Secondary, Ghost, Danger }

@Composable
fun FfButton(
    label: String,
    kind: ButtonKind,
    modifier: Modifier = Modifier,
    icon: DrawableResource? = null,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    val c = Theme.c
    val d = Theme.d
    val (fill, hover, press, ink) = when (kind) {
        ButtonKind.Primary -> listOf(c.accent, c.accentHover, c.accentPress, Color.White)
        ButtonKind.Secondary -> listOf(c.chipBg, c.chipBgHover, c.chipBgPress, c.textStrong)
        ButtonKind.Ghost -> listOf(Color.Transparent, c.chipBg, c.chipBgHover, c.text)
        ButtonKind.Danger -> listOf(Color.Transparent, c.errorSoft, c.errorSoftPress, c.error)
    }
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    val hovered by source.collectIsHoveredAsState()
    // A transparent fill keeps the hover colour's rgb so the fade doesn't pass through black.
    val rest = if (fill == Color.Transparent) hover.copy(alpha = 0f) else fill
    val bg by animateColorAsState(if (!enabled) rest else if (pressed) press else if (hovered) hover else rest, tween(120))
    val shape = RoundedCornerShape(d.radiusSm + 2.dp)
    var m = modifier.heightIn(min = d.controlHeight).widthIn(min = d.controlHeight).alpha(if (enabled) 1f else 0.45f)
        .clip(shape).background(bg)
    if (kind == ButtonKind.Secondary) m = m.border(BorderStroke(1.dp, c.borderStrong), shape)
    Row(
        m.hoverable(source).clickable(source, indication = null, enabled = enabled, onClick = onClick)
            .padding(horizontal = if (label.isEmpty()) 0.dp else d.buttonPadding),
        horizontalArrangement = Arrangement.spacedBy(if (d.narrow) 6.dp else d.space2, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (icon != null) Icon(icon, 18.dp, ink)
        if (label.isNotEmpty()) Txt(label, d.fontMd, ink, 600, align = TextAlign.Center)
    }
}

@Composable
fun PrimaryButton(label: String, modifier: Modifier = Modifier, icon: DrawableResource? = null, enabled: Boolean = true, onClick: () -> Unit) =
    FfButton(label, ButtonKind.Primary, modifier, icon, enabled, onClick)

@Composable
fun SecondaryButton(label: String, modifier: Modifier = Modifier, icon: DrawableResource? = null, enabled: Boolean = true, onClick: () -> Unit) =
    FfButton(label, ButtonKind.Secondary, modifier, icon, enabled, onClick)

@Composable
fun GhostButton(label: String, modifier: Modifier = Modifier, icon: DrawableResource? = null, enabled: Boolean = true, onClick: () -> Unit) =
    FfButton(label, ButtonKind.Ghost, modifier, icon, enabled, onClick)

@Composable
fun DangerButton(label: String, modifier: Modifier = Modifier, icon: DrawableResource? = null, enabled: Boolean = true, onClick: () -> Unit) =
    FfButton(label, ButtonKind.Danger, modifier, icon, enabled, onClick)

@Composable
fun IconButton(icon: DrawableResource, modifier: Modifier = Modifier, tint: Color = Theme.c.textMuted, danger: Boolean = false, size: Dp = Theme.d.controlHeight, onClick: () -> Unit) {
    val c = Theme.c
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    val hovered by source.collectIsHoveredAsState()
    val bg = if (pressed) (if (danger) c.errorSoftPress else c.chipBgHover)
    else if (hovered) (if (danger) c.errorSoft else c.chipBg) else Color.Transparent
    Box(
        modifier.size(size).clip(CircleShape).background(bg).hoverable(source).clickable(source, indication = null, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, if (isTouch) 20.dp else 17.dp, if (danger) c.error else tint)
    }
}

private val isTouch get() = uniffi.flipflop.isTouch()

/** On/off switch with a label and an explanation under it. */
@Composable
fun ToggleRow(label: String, hint: String, checked: Boolean, onToggled: (Boolean) -> Unit) {
    val c = Theme.c
    Row(
        Modifier.fillMaxWidth().heightIn(min = Theme.d.controlHeight).clickable(remember { MutableInteractionSource() }, null) { onToggled(!checked) }
            .padding(vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(Theme.d.space3),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Txt(label, Theme.d.fontMd, c.textStrong, 600)
            Txt(hint, Theme.d.fontSm, c.textMuted)
        }
        val track by animateColorAsState(if (checked) c.accent else c.offline)
        Box(Modifier.size(42.dp, 24.dp).clip(RoundedCornerShape(12.dp)).background(track).padding(3.dp)) {
            Box(
                Modifier.size(18.dp).align(if (checked) Alignment.CenterEnd else Alignment.CenterStart)
                    .clip(CircleShape).background(Color.White),
            )
        }
    }
}

/** Segmented tab strip. */
@Composable
fun TabStrip(labels: List<String>, keys: List<String>, current: String, onPicked: (String) -> Unit) {
    val c = Theme.c
    val shape = RoundedCornerShape(Theme.d.radius)
    Row(
        Modifier.fillMaxWidth().height(Theme.d.controlHeight + 8.dp).clip(shape).background(c.chipBg).padding(4.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        labels.forEachIndexed { i, label ->
            val on = current == keys[i]
            val inner = RoundedCornerShape(Theme.d.radiusSm + 2.dp)
            Box(
                Modifier.weight(1f).fillMaxHeight().clip(inner)
                    .background(if (on) c.cardBg else Color.Transparent)
                    .then(if (on) Modifier.border(1.dp, c.border, inner) else Modifier)
                    .clickable(remember { MutableInteractionSource() }, null) { onPicked(keys[i]) },
                contentAlignment = Alignment.Center,
            ) {
                Txt(label, Theme.d.fontMd, if (on) c.accentText else c.textMuted, if (on) 700 else 500, align = TextAlign.Center, elide = true)
            }
        }
    }
}

/** A text field styled like the cards, single line. */
@Composable
fun Field(
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    placeholder: String = "",
    readOnly: Boolean = false,
    onAccepted: ((String) -> Unit)? = null,
) {
    val c = Theme.c
    val d = Theme.d
    val shape = RoundedCornerShape(d.radiusSm + 2.dp)
    var focused by remember { androidx.compose.runtime.mutableStateOf(false) }
    Box(
        modifier.fillMaxWidth().heightIn(min = d.controlHeight).clip(shape).background(c.insetBg)
            .border(1.dp, if (focused) c.accent else c.borderStrong, shape).padding(horizontal = d.space3),
        contentAlignment = Alignment.CenterStart,
    ) {
        BasicTextField(
            value = value,
            onValueChange = onValueChange,
            readOnly = readOnly,
            singleLine = true,
            textStyle = TextStyle(color = c.textStrong, fontSize = d.fontMd),
            cursorBrush = SolidColor(c.accent),
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = { onAccepted?.invoke(value) }),
            modifier = Modifier.fillMaxWidth().onFocus { focused = it },
        )
        if (value.isEmpty() && !focused) Txt(placeholder, d.fontMd, c.textFaint, elide = true)
    }
}

/** A multi-line text field (snippets). */
@Composable
fun Area(value: String, onValueChange: (String) -> Unit, modifier: Modifier = Modifier, placeholder: String = "") {
    val c = Theme.c
    val d = Theme.d
    val shape = RoundedCornerShape(d.radiusSm + 2.dp)
    var focused by remember { androidx.compose.runtime.mutableStateOf(false) }
    Box(
        modifier.fillMaxWidth().heightIn(min = if (isTouch) 96.dp else 72.dp).clip(shape).background(c.insetBg)
            .border(1.dp, if (focused) c.accent else c.borderStrong, shape).padding(horizontal = d.space3, vertical = 8.dp),
    ) {
        BasicTextField(
            value = value,
            onValueChange = onValueChange,
            textStyle = TextStyle(color = c.textStrong, fontSize = d.fontMd),
            cursorBrush = SolidColor(c.accent),
            modifier = Modifier.fillMaxWidth().onFocus { focused = it },
        )
        if (value.isEmpty() && !focused) Txt(placeholder, d.fontMd, c.textFaint)
    }
}

fun Modifier.onFocus(f: (Boolean) -> Unit): Modifier = this.then(Modifier.onFocusChanged { f(it.isFocused) })
