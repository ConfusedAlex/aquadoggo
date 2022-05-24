use aquadoggo::db::connection_pool;
use aquadoggo::graphql::{build_root_schema, Context};

#[tokio::main]
async fn main() {
    let pool = connection_pool("sqlite::memory:", 1).await.unwrap();
    let context = Context::new(pool);
    let schema = build_root_schema(context);
    let sdl = schema.sdl();

    println!("{sdl}");
}
