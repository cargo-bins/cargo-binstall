use criterion::{black_box, criterion_group, criterion_main, Criterion};
use leon::{vals, Template};
use serde::Serialize;
use tinytemplate::TinyTemplate;

fn compare_impls(c: &mut Criterion) {
    const TEMPLATE: &str = "hello {name}! i am {age} years old. my goal is to {goal}. i like: {flower}, {music}, {animal}, {color}, {food}. i'm drinking {drink}";
    fn replace_fn(key: &str) -> Option<&str> {
        match key {
            "name" => Some("marcus"),
            "age" => Some("42"),
            "goal" => Some("primary"),
            "flower" => Some("lotus"),
            "music" => Some("jazz"),
            "animal" => Some("cat"),
            "color" => Some("blue"),
            "food" => Some("pizza"),
            "drink" => Some("coffee"),
            _ => None,
        }
    }

    #[derive(Serialize)]
    struct Context<'c> {
        name: &'c str,
        age: u8,
        goal: &'c str,
        flower: &'c str,
        music: &'c str,
        animal: &'c str,
        color: &'c str,
        food: &'c str,
        drink: &'c str,
    }

    let tt_context = Context {
        name: "marcus",
        age: 42,
        goal: "primary",
        flower: "lotus",
        music: "jazz",
        animal: "cat",
        color: "blue",
        food: "pizza",
        drink: "coffee",
    };

    c.bench_function("leon", |b| {
        b.iter(|| {
            let template = Template::parse(black_box(TEMPLATE)).unwrap();
            let output = template.render(&vals(replace_fn)).unwrap();
            black_box(output);
        })
    });

    c.bench_function("std, string replaces", |b| {
        b.iter(|| {
            let mut output = black_box(TEMPLATE).to_string();
            for (key, value) in [
                ("name", "marcus"),
                ("age", "42"),
                ("goal", "primary"),
                ("flower", "lotus"),
                ("music", "jazz"),
                ("animal", "cat"),
                ("color", "blue"),
                ("food", "pizza"),
                ("drink", "coffee"),
            ] {
                output = output.replace(&format!("{{{}}}", key), value);
            }
            black_box(output);
        })
    });

    c.bench_function("tiny template", |b| {
        b.iter(|| {
            let mut tt = TinyTemplate::new();
            tt.add_template("tmp", black_box(TEMPLATE)).unwrap();
            let output = tt.render("tmp", &tt_context).unwrap();
            black_box(output);
        })
    });
}

criterion_group!(compare, compare_impls);
criterion_main!(compare);
