//! The first typed provider contract; transport/session availability is independent of module state.
use crate::{registry::ServiceKey, RequestSlot, RequestState, StationStatus};
use std::{cell::RefCell, rc::Rc};
pub trait OverviewService {
    fn snapshot(&self) -> RequestState<StationStatus>;
}
pub const OVERVIEW: ServiceKey<dyn OverviewService> = ServiceKey::new("stationdeck.admin.overview", "1.0.0");
pub struct SharedOverview(pub Rc<RefCell<RequestSlot<StationStatus>>>);
impl OverviewService for SharedOverview {
    fn snapshot(&self) -> RequestState<StationStatus> { self.0.borrow().state.clone() }
}
