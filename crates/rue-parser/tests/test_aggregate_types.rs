//! Parser tests for aggregate types (structs, enums, arrays, tuples)

use anyhow::Result;
use rue_parser::parse_with_diagnostics;
use rue_snapshot::{Snapshot, SnapshotConfig};

fn assert_parser_snapshot(name: &str, source: &str) -> Result<()> {
    let result = parse_with_diagnostics(source, "test.rue");
    let output = format!("{:#?}", result);

    Snapshot::with_config(name, SnapshotConfig::default()).assert(&output)?;

    Ok(())
}

// ===== Struct Tests =====

#[test]
fn test_struct_definition() -> Result<()> {
    assert_parser_snapshot(
        "struct_basic",
        r#"
struct Empty {}

struct Point {
    x: i32,
    y: i32,
}

struct Person {
    name: String,
    age: u32,
    active: bool,
}

struct Complex {
    id: u64,
    data: [i32; 10],
    metadata: (String, bool, i32),
}
"#,
    )
}

#[test]
fn test_struct_instantiation() -> Result<()> {
    assert_parser_snapshot(
        "struct_instantiation",
        r#"
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let origin = Point { x: 0, y: 0 };
    let p1 = Point { x: 10, y: 20 };
    
    // Struct with expression fields
    let p2 = Point { 
        x: p1.x + 5,
        y: p1.y * 2,
    };
    
    p2.x + p2.y
}
"#,
    )
}

#[test]
fn test_struct_field_access() -> Result<()> {
    assert_parser_snapshot(
        "struct_field_access",
        r#"
struct Nested {
    value: i32,
}

struct Container {
    nested: Nested,
    array: [i32; 3],
}

fn main() -> i32 {
    let c = Container {
        nested: Nested { value: 42 },
        array: [1, 2, 3],
    };
    
    // Simple field access
    let v = c.nested.value;
    
    // Chained field access
    let container2 = Container {
        nested: Nested { value: c.nested.value + 10 },
        array: c.array,
    };
    
    container2.nested.value + container2.array[0]
}
"#,
    )
}

// ===== Enum Tests =====

#[test]
fn test_enum_definition() -> Result<()> {
    assert_parser_snapshot(
        "enum_basic",
        r#"
enum Simple {
    First,
    Second,
    Third,
}

enum Option<T> {
    None,
    Some(T),
}

enum Result<T, E> {
    Ok(T),
    Err(E),
}

enum Message {
    Quit,
    Move { x: i32, y: i32 },
    Write(String),
    ChangeColor(u8, u8, u8),
}
"#,
    )
}

#[test]
fn test_enum_usage() -> Result<()> {
    assert_parser_snapshot(
        "enum_usage",
        r#"
enum Option<T> {
    None,
    Some(T),
}

fn main() -> i32 {
    let x = Option::Some(42);
    let y = Option::None;
    
    match x {
        Option::Some(val) => val,
        Option::None => 0,
    }
}
"#,
    )
}

#[test]
fn test_match_expressions() -> Result<()> {
    assert_parser_snapshot(
        "match_expressions",
        r#"
enum Color {
    Red,
    Green,
    Blue,
    RGB(u8, u8, u8),
}

fn main() -> i32 {
    let color = Color::RGB(255, 0, 128);
    
    let value = match color {
        Color::Red => 1,
        Color::Green => 2,
        Color::Blue => 3,
        Color::RGB(r, g, b) => (r + g + b) as i32,
    };
    
    // Match with guards
    let x = 10;
    match x {
        n if n < 0 => -1,
        0 => 0,
        n if n > 0 => 1,
        _ => 999,  // Unreachable but syntactically valid
    }
}
"#,
    )
}

// ===== Array Tests =====

#[test]
fn test_array_types() -> Result<()> {
    assert_parser_snapshot(
        "array_types",
        r#"
fn main() -> i32 {
    // Fixed-size arrays
    let arr1: [i32; 3] = [1, 2, 3];
    let arr2: [bool; 5] = [true, false, true, false, true];
    
    // Array of arrays
    let matrix: [[i32; 3]; 3] = [
        [1, 2, 3],
        [4, 5, 6],
        [7, 8, 9],
    ];
    
    // Array initialization with repeat syntax
    let zeros: [i32; 10] = [0; 10];
    
    arr1[0] + matrix[1][1] + zeros[5]
}
"#,
    )
}

#[test]
fn test_array_operations() -> Result<()> {
    assert_parser_snapshot(
        "array_operations",
        r#"
fn main() -> i32 {
    let mut arr = [1, 2, 3, 4, 5];
    
    // Array indexing
    let first = arr[0];
    let last = arr[4];
    
    // Array element assignment
    arr[2] = 100;
    
    // Complex array indexing
    let index = 2 + 1;
    let value = arr[index];
    
    // Nested array access
    let matrix = [[1, 2], [3, 4]];
    let elem = matrix[0][1];
    
    first + last + value + elem
}
"#,
    )
}

#[test]
fn test_slice_operations() -> Result<()> {
    assert_parser_snapshot(
        "slice_operations",
        r#"
fn main() -> i32 {
    let arr = [1, 2, 3, 4, 5];
    
    // Slice syntax
    let slice1 = &arr[1..3];  // Elements 1 and 2
    let slice2 = &arr[..2];   // First 2 elements
    let slice3 = &arr[3..];   // From element 3 to end
    let slice4 = &arr[..];    // Entire array as slice
    
    // Slice patterns in match
    match arr {
        [first, .., last] => first + last,
        _ => 0,
    }
}
"#,
    )
}

// ===== Tuple Tests =====

#[test]
fn test_tuple_types() -> Result<()> {
    assert_parser_snapshot(
        "tuple_types",
        r#"
fn main() -> i32 {
    // Simple tuples
    let pair: (i32, i32) = (10, 20);
    let triple: (bool, i32, String) = (true, 42, "hello");
    
    // Unit type (empty tuple)
    let unit: () = ();
    
    // Nested tuples
    let nested: ((i32, i32), (bool, bool)) = ((1, 2), (true, false));
    
    // Tuple destructuring
    let (x, y) = pair;
    let (flag, value, _text) = triple;
    let ((a, b), (c, d)) = nested;
    
    x + y + value + a + b
}
"#,
    )
}

#[test]
fn test_tuple_access() -> Result<()> {
    assert_parser_snapshot(
        "tuple_access",
        r#"
fn main() -> i32 {
    let tuple = (10, 20, 30, 40, 50);
    
    // Tuple indexing
    let first = tuple.0;
    let second = tuple.1;
    let last = tuple.4;
    
    // Nested tuple access
    let nested = ((1, 2), (3, 4));
    let val = nested.0.1;  // Should be 2
    
    first + second + last + val
}
"#,
    )
}

// ===== Generic Tests =====

#[test]
fn test_generic_functions() -> Result<()> {
    assert_parser_snapshot(
        "generic_functions",
        r#"
fn identity<T>(x: T) -> T {
    x
}

fn swap<T, U>(pair: (T, U)) -> (U, T) {
    let (a, b) = pair;
    (b, a)
}

fn map_option<T, U, F>(opt: Option<T>, f: F) -> Option<U>
where
    F: Fn(T) -> U,
{
    match opt {
        Option::Some(x) => Option::Some(f(x)),
        Option::None => Option::None,
    }
}

fn main() -> i32 {
    let x = identity(42);
    let pair = swap((10, true));
    x + pair.0 as i32
}
"#,
    )
}

#[test]
fn test_generic_structs() -> Result<()> {
    assert_parser_snapshot(
        "generic_structs",
        r#"
struct Pair<T, U> {
    first: T,
    second: U,
}

struct Box<T> {
    value: T,
}

impl<T> Box<T> {
    fn new(value: T) -> Box<T> {
        Box { value }
    }
    
    fn get(&self) -> &T {
        &self.value
    }
}

fn main() -> i32 {
    let pair = Pair { first: 10, second: true };
    let boxed = Box::new(42);
    
    pair.first + boxed.get()
}
"#,
    )
}

// ===== Complex Nested Types =====

#[test]
fn test_complex_nested_types() -> Result<()> {
    assert_parser_snapshot(
        "complex_nested_types",
        r#"
struct Database {
    tables: Vec<Table>,
    indices: HashMap<String, Index>,
}

struct Table {
    name: String,
    columns: Vec<Column>,
    rows: Vec<Row>,
}

struct Column {
    name: String,
    type: DataType,
    nullable: bool,
}

enum DataType {
    Integer,
    Float,
    String(usize),  // Max length
    Binary(Vec<u8>),
    Array(Box<DataType>),
}

type Row = Vec<Option<Value>>;

enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
}

fn main() -> i32 {
    let db = Database {
        tables: vec![
            Table {
                name: "users",
                columns: vec![
                    Column { name: "id", type: DataType::Integer, nullable: false },
                    Column { name: "name", type: DataType::String(100), nullable: false },
                ],
                rows: vec![],
            }
        ],
        indices: HashMap::new(),
    };
    
    42
}
"#,
    )
}

// ===== Type Alias Tests =====

#[test]
fn test_type_aliases() -> Result<()> {
    assert_parser_snapshot(
        "type_aliases",
        r#"
type Int = i32;
type Point = (i32, i32);
type Matrix = [[f64; 4]; 4];
type ResultInt = Result<i32, String>;

type Callback<T> = fn(T) -> T;
type Predicate<T> = fn(&T) -> bool;

fn main() -> Int {
    let p: Point = (10, 20);
    let matrix: Matrix = [[0.0; 4]; 4];
    let result: ResultInt = Result::Ok(42);
    
    let double: Callback<i32> = |x| x * 2;
    let is_even: Predicate<i32> = |x| x % 2 == 0;
    
    p.0 + p.1
}
"#,
    )
}

// ===== Pattern Matching Tests =====

#[test]
fn test_pattern_matching() -> Result<()> {
    assert_parser_snapshot(
        "pattern_matching",
        r#"
enum List<T> {
    Nil,
    Cons(T, Box<List<T>>),
}

fn main() -> i32 {
    let list = List::Cons(1, Box::new(List::Cons(2, Box::new(List::Nil))));
    
    // Match on enum variants
    match list {
        List::Nil => 0,
        List::Cons(head, tail) => {
            match *tail {
                List::Nil => head,
                List::Cons(second, _) => head + second,
            }
        }
    }
}
"#,
    )
}

#[test]
fn test_destructuring_patterns() -> Result<()> {
    assert_parser_snapshot(
        "destructuring_patterns",
        r#"
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let point = Point { x: 10, y: 20 };
    
    // Struct destructuring
    let Point { x, y } = point;
    let Point { x: a, y: b } = point;  // With renaming
    
    // Tuple destructuring
    let tuple = (1, 2, 3);
    let (first, second, third) = tuple;
    let (x, _, z) = tuple;  // Ignore middle element
    
    // Array destructuring
    let arr = [1, 2, 3, 4, 5];
    let [a, b, rest @ ..] = arr;
    let [first, .., last] = arr;
    
    x + y + first + last
}
"#,
    )
}
