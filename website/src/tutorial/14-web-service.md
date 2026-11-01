# A web service: HTTP, JSON, and PostgreSQL

The last chapter's guest book kept its data in a file. Real services keep it in a database. This chapter walks through [`examples/todo_api.kn`](https://github.com/Cody-will/kiln/blob/main/examples/todo_api.kn), a JSON REST API backed by PostgreSQL that Kiln's CI builds and tests end to end on every push.

You need a PostgreSQL server. With Docker:

```sh
docker run -d --name pg -e POSTGRES_USER=kiln -e POSTGRES_PASSWORD=secretpw -p 5432:5432 postgres:17
```

## Typed JSON with `@derive(Json)`

Deriving `Json` writes the encoder and decoder for a type. Fields become object keys; an `option` field may be missing or `null`:

```kiln
import std::encoding::json

@derive(Json, Debug, Clone)
struct Todo {
	id i64,
	title string,
	done bool,
}

@derive(Json, Debug)
struct Update {
	title option<string>,
	done option<bool>,
}

fun main() {
	t := Todo { id: 1, title: "write the spec".to_string(), done: false }
	println(json::to_string(&t)) // {"id":1,"title":"write the spec","done":false}
	let u Update = json::from_str("{{\"done\":true}}").unwrap()
	println("{u.done.unwrap()} {u.title.is_none()}") // true true
}
```

(`{{` and `}}` are literal braces in a Kiln string, which otherwise interpolates `{expr}`.)

## Talking to PostgreSQL

`std::db::postgres` speaks the PostgreSQL wire protocol directly; no C library is needed. Parameters are passed separately from the SQL (`$1`, `$2`, ...), so values are never spliced into the query text:

```kiln
import std::db::postgres

fun insert_todo(title &str) result<Todo, postgres::Error> {
	db mut := postgres::Conn::connect_url("postgres://kiln:secretpw@127.0.0.1:5432/postgres")?
	rs := db.query("INSERT INTO todos (title) VALUES ($1) RETURNING id, title, done", &[postgres::text(title)])?
	r := &rs.rows()[0]
	return Ok(Todo { id: r.get_i64(0)?, title: r.get_string(1)?, done: r.get_bool(2)? })
}
```

`query` returns rows; `execute` returns the number of rows affected; `batch_execute` runs several statements (handy for migrations). For a long-running service, `postgres::Pool::new(cfg, max)` shares connections between requests.

## Routing requests

An `http::Router` maps paths to handler functions. A pattern ending in `/` matches every path under it, which is how `/todos/{id}` is handled:

```kiln
fun main() result<void, io::Error> {
	router mut := http::Router::new()
	router.get("/health", health)
	router.get("/todos", list)
	router.post("/todos", create)
	router.handle("/todos/", one) // GET, PUT, DELETE /todos/{id}
	return router.listen("127.0.0.1:8080")
}
```

Handlers take an `http::Request` and return an `http::Response`. Errors from the database become a `500` with a JSON body, bad input a `400` or `422`:

```kiln
fun create(req http::Request) http::Response {
	let body NewTodo = match json::from_str(&req.body_text()) {
		Ok(b) => b,
		Err(e) => return error(400, &e.to_string()),
	}
	if body.title.trim().is_empty() {
		return error(422, "title must not be empty")
	}
	match insert_todo(&body.title) {
		Ok(t) => return http::Response::json(201, &json::to_string(&t)),
		Err(e) => return db_error(e),
	}
}
```

## Running it

```sh
DATABASE_URL=postgres://kiln:secretpw@127.0.0.1:5432/postgres kiln run examples/todo_api.kn
curl -s -X POST localhost:8080/todos -d '{"title":"try Kiln"}'
# {"id":1,"title":"try Kiln","done":false}
curl -s -X PUT localhost:8080/todos/1 -d '{"done":true}'
curl -s localhost:8080/todos
```

`examples/todo_api_test.sh` drives every route with `curl` and checks the answers; CI runs it against a `postgres:17` service container. Try adding a route, such as `DELETE /todos` to clear finished items, and a check for it.
