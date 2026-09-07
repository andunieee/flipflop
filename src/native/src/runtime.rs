use iroh::endpoint::Endpoint;
use iroh::protocol::Router;

pub struct NodeRuntime {
    pub endpoint: Endpoint,
    pub router: Router,
}
