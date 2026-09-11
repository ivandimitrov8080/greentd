use bevy::prelude::*;

#[derive(Component)]
struct Name(String);
#[derive(Component)]
struct Person;

#[derive(Resource)]
struct GreetTimer(Timer);

fn add_people(mut commands: Commands) {
    commands.spawn((Person, Name("Ivan".to_string())));
    commands.spawn((Person, Name("John".to_string())));
    commands.spawn((Person, Name("Bronn".to_string())));
}
fn update_people(mut query: Query<&mut Name, With<Person>>) {
    for mut name in &mut query {
        if name.0 == "Ivan" {
            name.0 = "Dragan".to_string();
            break;
        }
    }
}

fn greet_people(time: Res<Time>, mut timer: ResMut<GreetTimer>, query: Query<&Name, With<Person>>) {
    if timer.0.tick(time.delta()).is_finished() {
        for name in &query {
            println!("Hello, {}", name.0);
        }
    }
}

pub struct HelloPlugin;

impl Plugin for HelloPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(GreetTimer(Timer::from_seconds(2.0, TimerMode::Repeating)));
        app.add_systems(Update, (update_people, greet_people).chain());
        app.add_systems(Startup, add_people);
    }
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(HelloPlugin)
        .run();
}
