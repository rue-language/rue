+++
title = "Enums"
weight = 8
template = "tutorial/page.html"
+++

# Enums

Enums define types with a fixed set of possible values, called variants.

## Defining Enums

```rue check
enum Color {
    Red,
    Green,
    Blue,
}

fn main() -> i32 {
    let c = Color.Green;
    0
}
```

Variants are accessed with a `.`: `EnumName.VariantName`.

## Matching on Enums

Use `match` to handle each variant:

```rue check
enum Color {
    Red,
    Green,
    Blue,
}

fn color_value(c: Color) -> i32 {
    match c {
        Color.Red => 1,
        Color.Green => 2,
        Color.Blue => 3,
    }
}

fn main() -> i32 {
    println(@to_string(color_value(Color.Red)));    // prints: 1
    println(@to_string(color_value(Color.Green)));  // prints: 2
    println(@to_string(color_value(Color.Blue)));   // prints: 3
    0
}
```

## Exhaustive Matching

Match expressions on enums must be exhaustive—you must handle all variants:

```rue check
enum Direction {
    North,
    South,
    East,
    West,
}

fn to_degrees(d: Direction) -> i32 {
    match d {
        Direction.North => 0,
        Direction.East => 90,
        Direction.South => 180,
        Direction.West => 270,
    }
}

fn main() -> i32 {
    to_degrees(Direction.North)
}
```

If you forget a variant, the compiler will tell you:

```rue skip
fn to_degrees(d: Direction) -> i32 {
    match d {
        Direction.North => 0,
        Direction.East => 90,
        // Error: non-exhaustive match, missing South and West
    }
}
```

## Enums in Structs

Enums can be fields in structs:

```rue check
enum Status {
    Pending,
    Active,
    Completed,
}

struct Task {
    id: i32,
    status: Status,
}

fn is_done(task: Task) -> bool {
    match task.status {
        Status.Completed => true,
        _ => false,
    }
}

fn main() -> i32 {
    let task = Task {
        id: 1,
        status: Status.Active,
    };

    if is_done(task) {
        println("task is done");
    } else {
        println("task is not done");  // this branch runs: status is Active
    }
    0
}
```

## Example: Simple State Machine

```rue check
enum TrafficLight {
    Red,
    Yellow,
    Green,
}

fn next_light(current: TrafficLight) -> TrafficLight {
    match current {
        TrafficLight.Red => TrafficLight.Green,
        TrafficLight.Green => TrafficLight.Yellow,
        TrafficLight.Yellow => TrafficLight.Red,
    }
}

fn light_duration(light: TrafficLight) -> i32 {
    match light {
        TrafficLight.Red => 30,
        TrafficLight.Yellow => 5,
        TrafficLight.Green => 25,
    }
}

fn main() -> i32 {
    light_duration(next_light(TrafficLight.Red))
}
```
