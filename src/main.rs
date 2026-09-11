use bevy::prelude::*;

#[derive(Component)]
struct Name(String);
#[derive(Component)]
struct Person;

fn hello_world() {
    println!("hello, world")
}

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

fn greet_people(query: Query<&Name, With<Person>>) {
    for name in &query {
        println!("Hello, {}", name.0);
    }
}

pub struct HelloPlugin;

impl Plugin for HelloPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (hello_world, (update_people, greet_people).chain()));
        app.add_systems(Startup, add_people);
    }
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(HelloPlugin)
        .run();
}
