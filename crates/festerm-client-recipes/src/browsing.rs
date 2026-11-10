use crate::{AccountReference, HostReference, RecipeError};
use festerm_host_protocol::OperationId;

/// No availability claim: saved/cached host metadata is not evidence that a
/// host is online, trusted, authorized, or permits shell creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowseScope {
    Account(AccountReference),
    Host(HostReference),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowseTicket {
    request: OperationId,
    epoch: u64,
    scope: BrowseScope,
}

impl BrowseTicket {
    pub fn scope(&self) -> &BrowseScope {
        &self.scope
    }
}

/// A plan only. No executor exists until the verified host endpoint lands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowseRequest {
    FindHosts(BrowseTicket),
    InspectHost(BrowseTicket),
}

/// One explicit request at a time; no timer, fan-out or open/render effect.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct HostBrowser {
    epoch: u64,
    account: Option<AccountReference>,
    selected_host: Option<HostReference>,
    pending: Option<BrowseTicket>,
}

impl HostBrowser {
    pub fn select_account(&mut self, account: Option<AccountReference>) -> Result<(), RecipeError> {
        if let Some(account) = &account {
            account.validate()?;
        }
        self.cancel();
        self.account = account;
        self.selected_host = None;
        Ok(())
    }

    /// Local card/launcher opening does not call either request method.
    pub fn selected_host(&self) -> Option<&HostReference> {
        self.selected_host.as_ref()
    }

    pub fn find_hosts(&mut self) -> Result<BrowseRequest, RecipeError> {
        let account = self.account.clone().ok_or(RecipeError::Invalid)?;
        self.selected_host = None;
        Ok(BrowseRequest::FindHosts(
            self.begin(BrowseScope::Account(account)),
        ))
    }

    /// Selecting a host is a separate explicit request, never a fan-out
    /// response to a service inventory result.
    pub fn inspect_host(&mut self, host: HostReference) -> Result<BrowseRequest, RecipeError> {
        host.validate()?;
        if self.account.as_ref() != Some(&host.account) {
            return Err(RecipeError::Invalid);
        }
        self.selected_host = Some(host.clone());
        Ok(BrowseRequest::InspectHost(
            self.begin(BrowseScope::Host(host)),
        ))
    }

    pub fn refresh(&mut self) -> Result<BrowseRequest, RecipeError> {
        match self.selected_host.clone() {
            Some(host) => self.inspect_host(host),
            None => self.find_hosts(),
        }
    }

    pub fn cancel(&mut self) {
        self.epoch = self
            .epoch
            .checked_add(1)
            .expect("host browse epoch exhausted");
        self.pending = None;
    }

    /// A late result after cancellation, account switch or a newer request
    /// cannot update a browser. Actual result schemas remain protocol-owned.
    pub fn complete(&mut self, ticket: &BrowseTicket) -> bool {
        if self.pending.as_ref() != Some(ticket) {
            return false;
        }
        self.pending = None;
        true
    }

    pub fn pending(&self) -> Option<&BrowseTicket> {
        self.pending.as_ref()
    }

    fn begin(&mut self, scope: BrowseScope) -> BrowseTicket {
        self.cancel();
        let ticket = BrowseTicket {
            request: OperationId::new(),
            epoch: self.epoch,
            scope,
        };
        self.pending = Some(ticket.clone());
        ticket
    }
}
