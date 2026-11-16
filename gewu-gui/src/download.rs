//! Short-lived browser download tickets.
//!
//! Browser downloads cannot carry the WebSocket auth frame, and putting the
//! GUI token in a URL would leak it into history and logs. Instead the
//! authenticated browser socket mints an unguessable, expiring ticket that
//! the HTTP download route consumes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// How long a minted download URL stays valid.
const TICKET_TTL: Duration = Duration::from_secs(120);

#[derive(Debug)]
struct Ticket {
    session_id: String,
    path: String,
    expires: Instant,
}

/// In-memory ticket store shared by browser sessions and the HTTP route.
#[derive(Debug, Default)]
pub struct DownloadTickets {
    tickets: Mutex<HashMap<Uuid, Ticket>>,
}

impl DownloadTickets {
    /// Mint a ticket for a workspace-relative path in `session_id`.
    pub fn mint(&self, session_id: String, path: String) -> Uuid {
        let now = Instant::now();
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|_, ticket| ticket.expires > now);
        let id = Uuid::new_v4();
        tickets.insert(
            id,
            Ticket {
                session_id,
                path,
                expires: now + TICKET_TTL,
            },
        );
        id
    }

    /// Look up a live ticket, returning `(session_id, path)`.
    pub fn get(&self, id: &Uuid) -> Option<(String, String)> {
        let now = Instant::now();
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|_, ticket| ticket.expires > now);
        tickets
            .get(id)
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
    }

    #[test]
    fn expired_tickets_are_rejected() {
        let tickets = DownloadTickets::default();
        let id = tickets.mint("s1".into(), "src/main.rs".into());
        {
            let mut live = tickets.tickets.lock().unwrap();
            live.get_mut(&id).unwrap().expires = Instant::now() - Duration::from_secs(1);
        }
        assert_eq!(tickets.get(&id), None);
    }
}
