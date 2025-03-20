use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use warp::Filter;
use warp::ws::{Message, WebSocket};

const ROWS: usize = 6;
const COLS: usize = 7;

#[derive(Serialize, Deserialize, Clone)]
struct GameState {
    player1: Option<String>,
    player2: Option<String>,
    board: [[u8; COLS]; ROWS],
    turn: u8, // 1 for Player 1, 2 for Player 2
}

type Clients = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Message>>>>;
type Spectators = Arc<Mutex<Vec<String>>>;
type Game = Arc<tokio::sync::Mutex<GameState>>;

#[tokio::main]
async fn main() {
    // Récupération du port depuis les arguments
    let args: Vec<String> = env::args().collect();
    let port: u16 = args
        .get(1)
        .and_then(|p| p.parse().ok()) // Tente de convertir l'argument en nombre
        .unwrap_or(3000); // Utilise 3000 si l'argument est absent ou invalide

    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let spectators: Spectators = Arc::new(Mutex::new(Vec::new()));
    let game_state = Arc::new(Mutex::new(GameState {
        player1: None,
        player2: None,
        board: [[0; COLS]; ROWS],
        turn: 1,
    }));

    let clients_filter = warp::any().map(move || clients.clone());
    let spectators_filter = warp::any().map(move || spectators.clone());
    let game_filter = warp::any().map(move || game_state.clone());

    let ws_route = warp::path("p4")
        .and(warp::ws())
        .and(clients_filter)
        .and(spectators_filter)
        .and(game_filter)
        .map(|ws: warp::ws::Ws, clients, spectators, game| {
            ws.on_upgrade(move |socket| handle_connection(socket, clients, spectators, game))
        });

    let routes = ws_route.with(warp::cors().allow_any_origin());

    println!(
        "🚀 Serveur WebSocket en ligne sur ws://0.0.0.0:{}/ws",
        port
    );
    warp::serve(routes).run(([0, 0, 0, 0], port)).await;
}

async fn handle_connection(ws: WebSocket, clients: Clients, spectators: Spectators, game: Game) {
    let (mut tx, mut rx) = ws.split();
    let (client_sender, mut client_receiver) = mpsc::unbounded_channel();
    let client_id = uuid::Uuid::new_v4().to_string();

    let mut game_state = game.lock().await;
    let role = if game_state.player1.is_none() {
        game_state.player1 = Some(client_id.clone());
        "Joueur 1"
    } else if game_state.player2.is_none() {
        game_state.player2 = Some(client_id.clone());
        "Joueur 2"
    } else {
        spectators.lock().await.push(client_id.clone());
        "Spectateur"
    };
    drop(game_state);

    clients
        .lock()
        .await
        .insert(client_id.clone(), client_sender);
    println!("✅ {} connecté: {}", role, client_id);
    let _ = tx
        .send(Message::text(format!("Bienvenue, vous êtes {}", role)))
        .await;
    broadcast_game_state(&clients, &game).await;

    tokio::spawn(async move {
        while let Some(msg) = client_receiver.recv().await {
            let _ = tx.send(msg).await;
        }
    });

    while let Some(result) = rx.next().await {
        if let Ok(msg) = result {
            handle_message(client_id.clone(), msg, &clients, &spectators, &game).await;
        }
    }

    let mut game_state = game.lock().await;
    if game_state.player1.as_ref() == Some(&client_id) {
        game_state.player1 = None;
    } else if game_state.player2.as_ref() == Some(&client_id) {
        game_state.player2 = None;
    } else {
        spectators.lock().await.retain(|id| id != &client_id);
    }
    drop(game_state);
    clients.lock().await.remove(&client_id);
    println!("❌ Déconnecté: {}", client_id);
}

async fn handle_message(
    client_id: String,
    msg: Message,
    clients: &Clients,
    spectators: &Spectators,
    game: &Game,
) {
    if let Ok(text) = msg.to_str() {
        if let Ok(command) = serde_json::from_str::<usize>(text) {
            let mut game_state = game.lock().await;

            let is_spectator = spectators.lock().await.contains(&client_id);
            if is_spectator {
                return;
            }

            if let Some(winner) = make_move(command, &mut game_state, &client_id) {
                broadcast_message(clients, &format!("🎉 Victoire de Joueur {}!", winner)).await;
                println!("🎉 Victoire de Joueur {}!", winner);
                return;
            }

            if is_draw(&game_state.board) {
                broadcast_message(clients, "🤝 Match nul ! La grille est pleine.").await;
                println!("🤝 Match nul ! La grille est pleine.");
                return;
            }

            drop(game_state);
            broadcast_game_state(clients, game).await;
        }
    }
}

fn make_move(column: usize, game_state: &mut GameState, player_id: &String) -> Option<u8> {
    if column >= COLS {
        return None;
    }
    let player = if game_state.player1.as_ref() == Some(player_id) {
        1
    } else if game_state.player2.as_ref() == Some(player_id) {
        2
    } else {
        return None;
    };

    if player != game_state.turn {
        return None;
    }

    for row in (0..ROWS).rev() {
        if game_state.board[row][column] == 0 {
            game_state.board[row][column] = player;

            if check_winner(&game_state.board, row, column, player) {
                return Some(player);
            }

            game_state.turn = if game_state.turn == 1 { 2 } else { 1 };
            return None;
        }
    }
    None
}

fn check_winner(board: &[[u8; COLS]; ROWS], row: usize, col: usize, player: u8) -> bool {
    let directions = [(1, 0), (0, 1), (1, 1), (1, -1)];

    for &(dx, dy) in &directions {
        let mut count = 1;

        let mut r = row as isize + dx;
        let mut c = col as isize + dy;
        while r >= 0
            && r < ROWS as isize
            && c >= 0
            && c < COLS as isize
            && board[r as usize][c as usize] == player
        {
            count += 1;
            r += dx;
            c += dy;
        }

        let mut r = row as isize - dx;
        let mut c = col as isize - dy;
        while r >= 0
            && r < ROWS as isize
            && c >= 0
            && c < COLS as isize
            && board[r as usize][c as usize] == player
        {
            count += 1;
            r -= dx;
            c -= dy;
        }

        if count >= 4 {
            return true;
        }
    }
    false
}

fn is_draw(board: &[[u8; COLS]; ROWS]) -> bool {
    board.iter().all(|row| row.iter().all(|&cell| cell != 0))
}

async fn broadcast_game_state(clients: &Clients, game: &Game) {
    let game_state = game.lock().await;
    let mut grid = String::new();

    for row in game_state.board.iter() {
        grid.push_str("+---+---+---+---+---+---+---+\n|");
        for &cell in row.iter() {
            let symbol = match cell {
                1 => " X ",
                2 => " O ",
                _ => "   ",
            };
            grid.push_str(symbol);
            grid.push('|');
        }
        grid.push('\n');
    }
    grid.push_str("+---+---+---+---+---+---+---+\n");

    let turn_msg = if game_state.turn == 1 {
        "🟢 À ton tour, Joueur 1 !"
    } else {
        "🟢 À ton tour, Joueur 2 !"
    };

    let full_message = format!("{}\n{}", turn_msg, grid);

    let clients_lock = clients.lock().await;

    for (client_id, sender) in clients_lock.iter() {
        if (game_state.player1.as_ref() == Some(client_id) && game_state.turn == 1)
            || (game_state.player2.as_ref() == Some(client_id) && game_state.turn == 2)
        {
            // Envoyer uniquement au joueur dont c'est le tour
            let _ = sender.send(Message::text(format!(
                "{}🎯 Entrez un numéro de colonne (0-6) : ",
                full_message
            )));
        } else {
            // Envoyer la grille aux autres joueurs/spectateurs sans l'invite de jeu
            let _ = sender.send(Message::text(full_message.clone()));
        }
    }
}

async fn broadcast_message(clients: &Clients, message: &str) {
    let clients = clients.lock().await;
    for sender in clients.values() {
        let _ = sender.send(Message::text(message.to_string()));
    }
}
