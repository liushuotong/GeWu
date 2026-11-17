//! Short-lived browser file tickets.
//!
//! Browser downloads and previews cannot carry the WebSocket auth frame, and
//! putting the GUI token in a URL would leak it into history and logs. Instead
//! the authenticated browser socket mints an unguessable, expiring ticket that
//! the HTTP route consumes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// How long a minted download URL stays valid.
const TICKET_TTL: Duration = Duration::from_secs(120);

/// How long a minted preview URL stays valid. Longer than downloads so an open
/// preview window can be reloaded.
const PREVIEW_TTL: Duration = Duration::from_secs(3600);

/// What a ticket may be redeemed for; routes only accept their own kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TicketKind {
    Download,
    Preview,
}

#[derive(Debug)]
struct Ticket {
    session_id: String,
    path: String,
    kind: TicketKind,
    expires: Instant,
}

/// In-memory ticket store shared by browser sessions and the HTTP routes.
#[derive(Debug, Default)]
pub struct DownloadTickets {
    tickets: Mutex<HashMap<Uuid, Ticket>>,
}

impl DownloadTickets {
    /// Mint a ticket for a workspace-relative path in `session_id`.
    pub fn mint(&self, session_id: String, path: String) -> Uuid {
        self.insert(session_id, path, TicketKind::Download, TICKET_TTL)
    }

    /// Mint a ticket for an HTML preview of `path` in `session_id`.
    pub fn mint_preview(&self, session_id: String, path: String) -> Uuid {
        self.insert(session_id, path, TicketKind::Preview, PREVIEW_TTL)
    }

    /// Look up a live download ticket, returning `(session_id, path)`.
    pub fn get(&self, id: &Uuid) -> Option<(String, String)> {
        self.lookup(id, TicketKind::Download)
    }

    /// Look up a live preview ticket, returning `(session_id, path)`.
    pub fn get_preview(&self, id: &Uuid) -> Option<(String, String)> {
        self.lookup(id, TicketKind::Preview)
    }

    fn insert(&self, session_id: String, path: String, kind: TicketKind, ttl: Duration) -> Uuid {
        let now = Instant::now();
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|_, ticket| ticket.expires > now);
        let id = Uuid::new_v4();
        tickets.insert(
            id,
            Ticket {
                session_id,
                path,
                kind,
                expires: now + ttl,
            },
        );
        id
    }

    fn lookup(&self, id: &Uuid, kind: TicketKind) -> Option<(String, String)> {
        let now = Instant::now();
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|_, ticket| ticket.expires > now);
        tickets
            .get(id)
            .filter(|ticket| ticket.kind == kind)
            .map(|ticket| (ticket.session_id.clone(), ticket.path.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_tickets_resolve_to_their_target() {
        let tickets = DownloadTickets::default();
        let id = tickets.mint("s1".into(), "src/main.rs".into());
        assert_eq!(
            tickets.get(&id),
            Some(("s1".to_string(), "src/main.rs".to_string()))
        );
    }

    #[test]
    fn unknown_tickets_are_rejected() {
        let tickets = DownloadTickets::default();
        assert_eq!(tickets.get(&Uuid::new_v4()), None);
        assert_eq!(tickets.get_preview(&Uuid::new_v4()), None);
    }

    #[test]
    fn preview_tickets_resolve_to_their_target() {
        let tickets = DownloadTickets::default();
        let id = tickets.mint_preview("s1".into(), "docs/a.html".into());
        assert_eq!(
            tickets.get_preview(&id),
            Some(("s1".to_string(), "docs/a.html".to_string()))
        );
    }

    #[test]
    fn tickets_do_not_cross_route_kinds() {
        let tickets = DownloadTickets::default();
        let download = tickets.mint("s1".into(), "src/main.rs".into());
        let preview = tickets.mint_preview("s1".into(), "docs/a.html".into());
        assert_eq!(tickets.get_preview(&download), None);
        assert_eq!(tickets.get(&preview), None);
    }

    #[test]
    fn expired_tickets_are_rejected() {
        let tickets = DownloadTickets::default();
        let id = tickets.mint("s1".into(), "src/main.rs".into());
        let preview = tickets.mint_preview("s1".into(), "docs/a.html".into());
        {
            let mut live = tickets.tickets.lock().unwrap();
            live.get_mut(&id).unwrap().expires = Instant::now() - Duration::from_secs(1);
            live.get_mut(&preview).unwrap().expires = Instant::now() - Duration::from_secs(1);
        }
        assert_eq!(tickets.get(&id), None);
        assert_eq!(tickets.get_preview(&preview), None);
    }
}
