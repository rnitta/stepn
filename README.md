# STEPN

Tool to execute commands in sequence. 

## Install
`cargo install --git https://github.com/rnitta/stepn`

## Practical Usage
Assuming web development with Ruby on Rails and webpack(-dev-server).
When you run Rails app in local env, you must run commands below:

1. bundle exec rails db:migrate && bundle exec rails s
2. yarn start
3. docker-compose -f datastores.yml up

Now, Command 1 is dependent to Command 3, because the database migration cannot be done without datastores booted.
So, you should follow these steps manually:

- execute `docker-compose -f datastores.yml up`
- execute `yarn start`
- wait until docker-compose's startup process is done and the middlewares(such as postgresql, redis) is ready to accept connection.
- execute `bundle exec rails db:migrate && bundle exec rails s`

Irritating.

Write proc.toml and execute `stepn`.


## How it works
1
![1](./imgs/arc1.svg)

2
![2](./imgs/arc2.svg)

3
![3](./imgs/arc3.svg)

## proc.toml

### Config

| name     | required | default | type                     | explain                      |
| -------- | -------- | ------- | ------------------------ | ---------------------------- |
| services | yes      | -       | HashMap<String, Service> | list of service and its name |

### Service

| name           | required | default | type                    | explain                                                                        | 
| -------------- | -------- | ------- | ----------------------- | ------------------------------------------------------------------------------ | 
| command        | yes      | None    | String                  | command run in the service                                                     | 
| depends_on     | no       | None    | Vec<String>             | names of the other services waiting to be started when that service is started | 
| health_checker | no       | None    | HealthChecker           | conditions for certifying that the service has booted                          | 
| environments   | no       | None    | HashMap<String, String> | environment variables: <key, value>                                            | 
| delay_sec      | no       | None    | u64                     | seconds to wait before starting that service                                   | 

### HealthChecker

| name           | required | default | type        | explain                                                                        | 
| -------------- | -------- | ------- | ----------- | ------------------------------------------------------------------------------ | 
| output_trigger | no       | None    | Vec<String> | string to mark the service as booted if it appears in the log output to stdout | 



see `src/stepn_config.rs` for detail.

example1:

```proc.toml
[services.a]
command = "for i in {0..10}; do echo a$i; sleep 1; done"

# conditions of assuming the startup is done
[services.a.health_checker]
output_trigger = [
    "2",
    "5"
]

[services.b]
command = "for i in {0..10}; do echo b$i; sleep 1; done"
depends_on = ["a"]

[services.b.health_checker]
output_trigger = [
    "4"
]

[services.c]
command = "for i in {0..10}; do echo c$i; sleep 1; done"
depends_on = ["b"]
```

example2:

```proc.toml
[services.middleware]
command = "docker-compose --file dc-ds.yml up"

# conditions of assuming the startup is done
[services.middleware.health_check]
output_trigger = [
    "Ready to accept connections", # redis
    "database system is ready to accept connections" # postgresql
]

[services.web]
command = "bundle exec rails db:migrate && bundle exec rails s"
# migration can be executed after postgresql is booted.
depends_on = [ "middleware" ]
[services.web.environments]
PORT = "3000"
BINDING = "0.0.0.0"

[services.frontend]
command = "yarn webpack-dev-server"
```
