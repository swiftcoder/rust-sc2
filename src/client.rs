use crate::{
	api::API, bot::Bot, game_state::GameState, ids::AbilityId, paths::*, player::Computer, FromProtoData,
	IntoProto, IntoSC2, Player, PlayerSettings,
};
use num_traits::FromPrimitive;
use sc2_proto::{
	query::RequestQueryAvailableAbilities,
	sc2api::{PlayerSetup, PlayerType, PortSet, Request, RequestCreateGame, Status},
};
use std::{
	error::Error,
	fmt,
	fs::File,
	io::Write,
	ops::{Deref, DerefMut},
	panic,
	path::Path,
	process::{Child, Command},
	rc::Rc,
};
use tungstenite::{client::AutoStream, connect, WebSocket};
use url::Url;

pub type WS = WebSocket<AutoStream>;
pub type SC2Result<T> = Result<T, Box<dyn Error>>;

const HOST: &str = "127.0.0.1";
const PORT: i32 = 5000;

#[derive(Default)]
struct Human {
	process: Option<Child>,
	api: Option<API>,
}
impl Human {
	pub fn new() -> Self {
		Default::default()
	}
}
impl Drop for Human {
	fn drop(&mut self) {
		if let Some(api) = &mut self.api {
			let mut req = Request::new();
			req.mut_leave_game();
			if let Err(e) = api.send_request(req) {
				error!("Request LeaveGame failed: {}", e);
			}

			let mut req = Request::new();
			req.mut_quit();
			if let Err(e) = api.send_request(req) {
				error!("Request QuitGame failed: {}", e);
			}
		}

		if let Some(process) = &mut self.process {
			if let Err(e) = process.kill() {
				error!("Can't kill SC2 process: {}", e);
			}
		}
	}
}

#[derive(Debug)]
struct ProtoError(String);
impl ProtoError {
	fn new<E: fmt::Debug>(error: E, details: &str) -> Self {
		Self(format!("{:?}: {}", error, details))
	}
}
impl fmt::Display for ProtoError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.0)
	}
}
impl Error for ProtoError {}

struct Ports {
	shared: i32,
	server: (i32, i32),
	client: Vec<(i32, i32)>,
}

#[derive(Default)]
pub struct LaunchOptions<'a, P: AsRef<Path>> {
	pub sc2_version: Option<&'a str>,
	pub save_replay_as: Option<P>,
	pub realtime: bool,
}

// Runners
pub fn run_vs_computer<B, P>(
	bot: &mut B,
	computer: Computer,
	map_name: &str,
	options: LaunchOptions<P>,
) -> SC2Result<()>
where
	B: Player + DerefMut<Target = Bot> + Deref<Target = Bot>,
	P: AsRef<Path>,
{
	debug!("Starting game vs computer");

	let sc2_path = get_path_to_sc2();
	let map_path = get_map_path(&sc2_path, map_name);

	// let port = get_unused_port();
	debug!("Launching SC2 process");
	bot.process = Some(launch_client(&sc2_path, PORT, options.sc2_version)?);
	debug!("Connecting to websocket");
	bot.api = Some(API(connect_to_websocket(HOST, PORT)?));

	let settings = bot.get_player_settings();
	let api = &mut bot.api.as_mut().unwrap();

	debug!("Sending CreateGame request");
	// Create game
	let mut req = Request::new();
	let req_create_game = req.mut_create_game();

	req_create_game.mut_local_map().set_map_path(map_path);
	/*
	Set Map
		req.mut_create_game().mut_local_map().set_map_path("");
	OR
		req.mut_create_game().set_battlenet_map_name("");
	*/
	create_player_setup(&settings, req_create_game);
	create_computer_setup(computer, req_create_game);
	// req_create_game.set_disable_fog(bool); // Cheat
	// req_create_game.set_random_seed(u32);
	let realtime = options.realtime;
	req_create_game.set_realtime(realtime);

	let res = api.send(req)?;
	let res_create_game = res.get_create_game();
	if res_create_game.has_error() {
		let err = format!(
			"{:?}: {}",
			res_create_game.get_error(),
			res_create_game.get_error_details()
		);
		error!("{}", err);
		panic!(err);
	}

	debug!("Sending JoinGame request");
	bot.player_id = join_game(&settings, api, None)?;

	set_static_data(bot)?;

	debug!("Entered main loop");
	// Main loop
	play_first_step(bot, realtime)?;
	let mut iteration = 0;
	while play_step(bot, iteration, realtime)? {
		iteration += 1;
	}
	debug!("Game finished");

	if let Some(path) = options.save_replay_as {
		save_replay(bot.api(), path)?;
	}
	Ok(())
}

pub fn run_ladder_game<B>(
	bot: &mut B,
	host: &str,
	port: &str,
	player_port: i32,
	opponent_id: Option<&str>,
) -> SC2Result<()>
where
	B: Player + DerefMut<Target = Bot> + Deref<Target = Bot>,
{
	debug!("Starting ladder game");

	debug!("Connecting to websocket");
	bot.api = Some(API(connect_to_websocket(&host, port.parse()?)?));

	debug!("Sending JoinGame request");

	if let Some(id) = opponent_id {
		bot.opponent_id = id.to_string();
	}

	bot.player_id = join_game(
		&bot.get_player_settings(),
		bot.api(),
		Some(&Ports {
			shared: player_port + 1,
			server: (player_port + 2, player_port + 3),
			client: vec![(player_port + 4, player_port + 5)],
		}),
	)?;

	set_static_data(bot)?;

	debug!("Entered main loop");
	// Main loop
	let mut iteration = 0;
	play_first_step(bot, false)?;
	while play_step(bot, iteration, false)? {
		iteration += 1;
	}
	debug!("Game finished");

	Ok(())
}

pub fn run_vs_human<B, P>(
	bot: &mut B,
	human_settings: PlayerSettings,
	map_name: &str,
	options: LaunchOptions<P>,
) -> SC2Result<()>
where
	B: Player + DerefMut<Target = Bot> + Deref<Target = Bot>,
	P: AsRef<Path>,
{
	debug!("Starting human vs bot");
	let sc2_path = get_path_to_sc2();
	let map_path = get_map_path(&sc2_path, map_name);

	// let ports = get_unused_ports(9);
	// let (port_human, port_bot) = (ports[0], ports[1]);
	let (port_human, port_bot) = (PORT, PORT + 1);

	let mut human = Human::new();

	let sc2_version = options.sc2_version;
	debug!("Launching host SC2 process");
	human.process = Some(launch_client(&sc2_path, port_human, sc2_version)?);
	debug!("Launching client SC2 process");
	bot.process = Some(launch_client(&sc2_path, port_bot, sc2_version)?);
	debug!("Connecting to host websocket");
	human.api = Some(API(connect_to_websocket(HOST, port_human)?));
	debug!("Connecting to client websocket");
	bot.api = Some(API(connect_to_websocket(HOST, port_bot)?));
	let human_api = &mut human.api.as_mut().unwrap();

	debug!("Sending CreateGame request to host process");
	let mut req = Request::new();
	let req_create_game = req.mut_create_game();
	req_create_game.mut_local_map().set_map_path(map_path);
	create_player_setup(&human_settings, req_create_game);
	create_player_setup(&bot.get_player_settings(), req_create_game);
	// req_create_game.set_disable_fog(bool); // Cheat
	// req_create_game.set_random_seed(u32);
	let realtime = options.realtime;
	req_create_game.set_realtime(realtime);

	let res = human_api.send(req)?;
	let res_create_game = res.get_create_game();
	if res_create_game.has_error() {
		let err = format!(
			"{:?}: {}",
			res_create_game.get_error(),
			res_create_game.get_error_details()
		);
		error!("{}", err);
		panic!(err);
	}

	debug!("Sending JoinGame request to both processes");
	/*let ports = Ports {
		shared: ports[2],
		server: (ports[3], ports[4]),
		client: vec![(ports[5], ports[6]), (ports[7], ports[8])],
	};*/
	let ports = Ports {
		shared: PORT + 2,
		server: (PORT + 3, PORT + 4),
		client: vec![(PORT + 5, PORT + 6), (PORT + 7, PORT + 8)],
	};
	join_game2(&human_settings, human_api, Some(&ports))?;
	join_game2(&bot.get_player_settings(), bot.api(), Some(&ports))?;
	let _ = wait_join(human_api)?;
	bot.player_id = wait_join(bot.api())?;

	set_static_data(bot)?;

	debug!("Entered main loop");
	play_first_step(bot, realtime)?;
	let mut iteration = 0;
	while play_step(bot, iteration, realtime)? {
		iteration += 1;
	}
	debug!("Game finished");

	if let Some(path) = options.save_replay_as {
		save_replay(bot.api(), path)?;
	}
	Ok(())
}

// Mini Helpers
/*
fn get_unused_port() -> i32 {
	(5000..65535)
		.find(|port| TcpListener::bind((HOST, *port)).is_ok())
		.expect("Can't find available port") as i32
}

fn get_unused_ports(n: usize) -> Vec<i32> {
	let mut ports = Vec::new();
	for port in 5000..65535 {
		if TcpListener::bind((HOST, port)).is_ok() {
			ports.push(port as i32);
			if ports.len() >= n {
				break;
			}
		}
	}
	ports
}
*/

// Helpers
fn set_static_data(bot: &mut Bot) -> SC2Result<()> {
	let api = &mut bot.api.as_mut().expect("API is not initialized");

	debug!("Requesting GameInfo");
	let mut req = Request::new();
	req.mut_game_info();
	let mut res = api.send(req)?;
	bot.game_info = res.take_game_info().into_sc2();

	debug!("Requesting GameData");
	let mut req = Request::new();
	let req_game_data = req.mut_data();
	req_game_data.set_ability_id(true);
	req_game_data.set_unit_type_id(true);
	req_game_data.set_upgrade_id(true);
	req_game_data.set_buff_id(true);
	req_game_data.set_effect_id(true);
	let mut res = api.send(req)?;
	bot.game_data = Rc::new(res.take_data().into_sc2());
	Ok(())
}

fn create_player_setup(settings: &PlayerSettings, req_create_game: &mut RequestCreateGame) {
	let mut setup = PlayerSetup::new();

	setup.set_race(settings.race.into_proto());
	setup.set_field_type(PlayerType::Participant);
	if let Some(name) = &settings.name {
		setup.set_player_name(name.clone());
	}
	req_create_game.mut_player_setup().push(setup);
}

fn create_computer_setup(computer: Computer, req_create_game: &mut RequestCreateGame) {
	let mut setup = PlayerSetup::new();

	setup.set_race(computer.race.into_proto());
	setup.set_field_type(PlayerType::Computer);
	setup.set_difficulty(computer.difficulty.into_proto());
	if let Some(ai_build) = computer.ai_build {
		setup.set_ai_build(ai_build.into_proto());
	}
	req_create_game.mut_player_setup().push(setup);
}

fn join_game(settings: &PlayerSettings, api: &mut API, ports: Option<&Ports>) -> SC2Result<u32> {
	join_game2(settings, api, ports)?;
	wait_join(api)
}
fn join_game2(settings: &PlayerSettings, api: &mut API, ports: Option<&Ports>) -> SC2Result<()> {
	let mut req = Request::new();
	let req_join_game = req.mut_join_game();

	req_join_game.set_race(settings.race.into_proto());

	let options = req_join_game.mut_options();
	options.set_raw(true);
	options.set_score(true);
	// options.mut_feature_layer()
	// options.mut_render();
	options.set_show_cloaked(true);
	options.set_show_burrowed_shadows(true);
	options.set_show_placeholders(true);
	options.set_raw_affects_selection(settings.raw_affects_selection);
	// options.set_raw_crop_to_playable_area(bool);
	if let Some(name) = &settings.name {
		req_join_game.set_player_name(name.clone());
	}

	if let Some(ports) = ports {
		// Shared port is deprecated
		req_join_game.set_shared_port(ports.shared);

		let server_ports = req_join_game.mut_server_ports();
		server_ports.set_game_port(ports.server.0);
		server_ports.set_base_port(ports.server.1);

		let client_ports = req_join_game.mut_client_ports();
		ports.client.iter().for_each(|client| {
			let mut port_set = PortSet::new();
			port_set.set_game_port(client.0);
			port_set.set_base_port(client.1);
			client_ports.push(port_set);
		});
	}

	api.send_only(req)?;
	Ok(())
}
fn wait_join(api: &mut API) -> SC2Result<u32> {
	let res = api.wait_response()?;

	let res_join_game = res.get_join_game();
	if res_join_game.has_error() {
		let err = ProtoError::new(res_join_game.get_error(), res_join_game.get_error_details());
		error!("{}", err);
		Err(Box::new(err))
	} else {
		Ok(res_join_game.get_player_id())
	}
}

fn play_first_step<B>(bot: &mut B, realtime: bool) -> SC2Result<()>
where
	B: Player + DerefMut<Target = Bot> + Deref<Target = Bot>,
{
	let mut req = Request::new();
	req.mut_observation();

	let res = bot.api().send(req)?;

	bot.init_data_for_unit();
	bot.state = GameState::from_proto_data(bot.get_data_for_unit(), res.get_observation());
	bot.prepare_start();

	bot.on_start()?;

	let bot_actions = bot.get_actions();
	if !bot_actions.is_empty() {
		let mut req = Request::new();
		let actions = req.mut_action().mut_actions();
		bot_actions.into_iter().for_each(|a| actions.push(a.into_proto()));
		bot.clear_actions();
		bot.api().send_request(req)?;
	}
	if !realtime {
		let mut req = Request::new();
		req.mut_step().set_count(bot.game_step);
		bot.api().send_request(req)?;
	}
	Ok(())
}

fn play_step<B>(bot: &mut B, iteration: usize, realtime: bool) -> SC2Result<bool>
where
	B: Player + DerefMut<Target = Bot> + Deref<Target = Bot>,
{
	let mut req = Request::new();
	req.mut_observation();
	let res = bot.api().send(req)?;

	if matches!(res.get_status(), Status::ended) {
		let result = res.get_observation().get_player_result()[bot.player_id as usize - 1]
			.get_result()
			.into_sc2();
		debug!("Result for bot: {:?}", result);
		bot.on_end(result)?;
		return Ok(false);
	}

	let state = GameState::from_proto_data(bot.get_data_for_unit(), res.get_observation());

	let mut req = Request::new();
	let req_query_abilities = req.mut_query().mut_abilities();
	state.observation.raw.units.iter().for_each(|u| {
		if u.is_mine() {
			let mut req_unit = RequestQueryAvailableAbilities::new();
			req_unit.set_unit_tag(u.tag);
			req_query_abilities.push(req_unit);
		}
	});

	let res = bot.api().send(req)?;
	bot.abilities_units = res
		.get_query()
		.get_abilities()
		.iter()
		.map(|a| {
			(
				a.get_unit_tag(),
				a.get_abilities()
					.iter()
					.filter_map(|ab| AbilityId::from_i32(ab.get_ability_id()))
					.collect(),
			)
		})
		.collect();

	bot.state = state;
	bot.prepare_step();

	bot.on_step(iteration)?;

	let bot_actions = bot.get_actions();
	if !bot_actions.is_empty() {
		// println!("{:?}: {:?}", iteration, bot_actions);
		let mut req = Request::new();
		let actions = req.mut_action().mut_actions();
		bot_actions.into_iter().for_each(|a| actions.push(a.into_proto()));
		bot.clear_actions();
		bot.api().send_request(req)?;
		/*
		let res = api.send(req);
		let results = res.get_action().get_result();
		if !results.is_empty() {
			println!("action_results: {:?}", results);
		}
		*/
	}

	let bot_debug_commands = bot.get_debug_commands();
	if !bot_debug_commands.is_empty() {
		let mut req = Request::new();
		let debug_commands = req.mut_debug().mut_debug();
		bot_debug_commands
			.into_iter()
			.for_each(|cmd| debug_commands.push(cmd.into_proto()));
		bot.clear_debug_commands();
		bot.api().send_request(req)?;
	}
	if !realtime {
		let mut req = Request::new();
		req.mut_step().set_count(bot.game_step);
		bot.api().send_request(req)?;
	}
	Ok(true)
}

fn save_replay<P: AsRef<Path>>(api: &mut API, path: P) -> SC2Result<()> {
	let mut req = Request::new();
	req.mut_save_replay();

	let res = api.send(req)?;

	let mut path = path.as_ref().to_path_buf();
	if !path.ends_with(".SC2Replay") {
		path.push(".SC2Replay");
	}
	let mut file = File::create(path)?;
	file.write_all(res.get_save_replay().get_data())?;
	Ok(())
}

fn launch_client(sc2_path: &str, port: i32, sc2_version: Option<&str>) -> SC2Result<Child> {
	let (base_version, data_hash) = match sc2_version {
		Some(ver) => get_version_info(ver),
		None => (get_latest_base_version(sc2_path), ""),
	};
	let (sc2_binary, sc2_support) = {
		if cfg!(target_os = "windows") {
			if cfg!(target_arch = "x86_64") {
				("SC2_x64.exe", "Support64")
			} else if cfg!(target_arch = "x86") {
				("SC2.exe", "Support")
			} else {
				panic!("Unsupported Arch");
			}
		} else if cfg!(target_os = "linux") {
			if cfg!(target_arch = "x86_64") {
				("SC2_x64", "Support64")
			} else if cfg!(target_arch = "x86") {
				("SC2", "Support")
			} else {
				panic!("Unsupported Arch");
			}
		} else {
			panic!("Unsupported OS");
		}
	};

	let mut process = Command::new(format!(
		"{}/Versions/Base{}/{}",
		sc2_path, base_version, sc2_binary
	));
	process
		.current_dir(format!("{}/{}", sc2_path, sc2_support))
		.arg("-listen")
		.arg(HOST)
		.arg("-port")
		.arg(port.to_string())
		// 0 - windowed, 1 - fullscreen
		.arg("-displayMode")
		.arg("0");
	if !data_hash.is_empty() {
		process.arg("-dataVersion").arg(data_hash);
	}
	Ok(process.spawn().expect("Can't launch SC2 process."))
}

fn connect_to_websocket(host: &str, port: i32) -> SC2Result<WS> {
	let url = Url::parse(format!("ws://{}:{}/sc2api", host, port).as_str())?;
	let (ws, _rs) = loop {
		if let Ok(result) = connect(url.clone()) {
			break result;
		}
	};
	Ok(ws)
}
