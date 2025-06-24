pub mod add;
pub mod change;
pub mod exists;
pub mod get;
pub mod remove;

use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
pub struct DataService {
    pub pool: Arc<SqlitePool>,
}

pub use add::*;
pub use change::*;
pub use exists::*;
pub use get::*;
pub use remove::*;

// here is just myself brain storming
//
// so , ill write the data type here
// user
//  name
//  id
//  list of friends
//  list of servers
//  icon 'just a nerd font icon, like '
//  date of creation ofc
// servers
//  name
//  id
//  list of users
//      user_id
//      role (admin or not, just to keep it simple)
//  date of creation
//  creator
//  banned_users
//      user_id
//      cause
//  timed users
//      user_id
//      when they where kicked
//      cause
//      time
//      the end of the timeout
// channels
//  id
//  name
//  messages
//      user_id
//      time
//      content
//      is_replaying
//      user id the message is replaying for
//      id
// friends chat
//  user_id 1 and user_id 2
//  messages
