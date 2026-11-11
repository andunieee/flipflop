//! The line icons (`app/icons`, 24 px, stroked).

/// An icon's SVG.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Icon {
    AddPeer,
    Apps,
    ArrowDown,
    ArrowLeft,
    ChevronDown,
    ChevronRight,
    Clipboard,
    Close,
    Copy,
    Edit,
    File,
    Folder,
    Groups,
    Image,
    Inbox,
    Link,
    Open,
    Peers,
    Person,
    Qr,
    Send,
    Settings,
    Share,
    Trash,
}

impl Icon {
    pub fn svg(self) -> &'static [u8] {
        match self {
            Icon::AddPeer => include_bytes!("../icons/add-peer.svg"),
            Icon::Apps => include_bytes!("../icons/apps.svg"),
            Icon::ArrowDown => include_bytes!("../icons/arrow-down.svg"),
            Icon::ArrowLeft => include_bytes!("../icons/arrow-left.svg"),
            Icon::ChevronDown => include_bytes!("../icons/chevron-down.svg"),
            Icon::ChevronRight => include_bytes!("../icons/chevron-right.svg"),
            Icon::Clipboard => include_bytes!("../icons/clipboard.svg"),
            Icon::Close => include_bytes!("../icons/close.svg"),
            Icon::Copy => include_bytes!("../icons/copy.svg"),
            Icon::Edit => include_bytes!("../icons/edit.svg"),
            Icon::File => include_bytes!("../icons/file.svg"),
            Icon::Folder => include_bytes!("../icons/folder.svg"),
            Icon::Groups => include_bytes!("../icons/groups.svg"),
            Icon::Image => include_bytes!("../icons/image.svg"),
            Icon::Inbox => include_bytes!("../icons/inbox.svg"),
            Icon::Link => include_bytes!("../icons/link.svg"),
            Icon::Open => include_bytes!("../icons/open.svg"),
            Icon::Peers => include_bytes!("../icons/peers.svg"),
            Icon::Person => include_bytes!("../icons/person.svg"),
            Icon::Qr => include_bytes!("../icons/qr.svg"),
            Icon::Send => include_bytes!("../icons/send.svg"),
            Icon::Settings => include_bytes!("../icons/settings.svg"),
            Icon::Share => include_bytes!("../icons/share.svg"),
            Icon::Trash => include_bytes!("../icons/trash.svg"),
        }
    }
}

pub const LOGO: &[u8] = include_bytes!("../icons/logo.png");
