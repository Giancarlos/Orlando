use orlando_core::GrainContext;
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

// ── ChatRoom grain ──────────────────────────────────────────────

#[derive(Default)]
struct ChatRoomState {
    messages: Vec<(String, String)>, // (user, text)
    participants: Vec<String>,
}

#[grain(state = ChatRoomState)]
struct ChatRoom;

#[message(result = ())]
struct JoinRoom {
    user: String,
}

#[message(result = ())]
struct SendChat {
    user: String,
    text: String,
}

#[message(result = Vec<(String, String)>)]
struct GetHistory;

#[grain_handler(ChatRoom)]
async fn handle_join(state: &mut ChatRoomState, msg: JoinRoom, _ctx: &GrainContext) {
    if !state.participants.contains(&msg.user) {
        println!("  [general] {} joined the room", msg.user);
        state.participants.push(msg.user);
    }
}

#[grain_handler(ChatRoom)]
async fn handle_send(state: &mut ChatRoomState, msg: SendChat, _ctx: &GrainContext) {
    println!("  [general] {}: {}", msg.user, msg.text);
    state.messages.push((msg.user, msg.text));
}

#[grain_handler(ChatRoom)]
async fn handle_history(
    state: &mut ChatRoomState,
    _msg: GetHistory,
    _ctx: &GrainContext,
) -> Vec<(String, String)> {
    state.messages.clone()
}

// ── UserProfile grain (shows grain-to-grain calls) ──────────────

#[derive(Default)]
struct UserProfileState {
    name: String,
    room_count: u32,
}

#[grain(state = UserProfileState)]
struct UserProfile;

#[message(result = ())]
struct SetName {
    name: String,
}

#[message(result = String)]
struct GetName;

#[message(result = ())]
struct JoinAndGreet {
    room_key: String,
}

#[grain_handler(UserProfile)]
async fn handle_set_name(state: &mut UserProfileState, msg: SetName, _ctx: &GrainContext) {
    state.name = msg.name;
}

#[grain_handler(UserProfile)]
async fn handle_get_name(
    state: &mut UserProfileState,
    _msg: GetName,
    _ctx: &GrainContext,
) -> String {
    state.name.clone()
}

#[grain_handler(UserProfile)]
async fn handle_join_and_greet(
    state: &mut UserProfileState,
    msg: JoinAndGreet,
    ctx: &GrainContext,
) {
    let room = ctx.get_ref::<ChatRoom>(&msg.room_key);
    room.ask(JoinRoom {
        user: state.name.clone(),
    })
    .await
    .unwrap();
    room.ask(SendChat {
        user: state.name.clone(),
        text: "Hello everyone!".into(),
    })
    .await
    .unwrap();
    state.room_count += 1;
}

// ── Main ────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let silo = Silo::new();

    // Set up user profiles
    let alice = silo.get_ref::<UserProfile>("alice");
    alice
        .ask(SetName {
            name: "Alice".into(),
        })
        .await
        .unwrap();

    let bob = silo.get_ref::<UserProfile>("bob");
    bob.ask(SetName {
        name: "Bob".into(),
    })
    .await
    .unwrap();

    // Users join a chat room (grain-to-grain call)
    println!("Users joining #general...");
    alice
        .ask(JoinAndGreet {
            room_key: "general".into(),
        })
        .await
        .unwrap();
    bob.ask(JoinAndGreet {
            room_key: "general".into(),
        })
        .await
        .unwrap();

    // Send more messages directly
    let room = silo.get_ref::<ChatRoom>("general");
    room.ask(SendChat {
        user: "Alice".into(),
        text: "How's everyone doing?".into(),
    })
    .await
    .unwrap();
    room.ask(SendChat {
        user: "Bob".into(),
        text: "Great, thanks!".into(),
    })
    .await
    .unwrap();

    // Fetch history
    let history = room.ask(GetHistory).await.unwrap();
    println!("\nChat history ({} messages):", history.len());
    for (user, text) in &history {
        println!("  {user}: {text}");
    }
}
