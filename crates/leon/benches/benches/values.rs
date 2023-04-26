use std::{borrow::Cow, collections::HashMap, sync::Arc};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use leon::{vals, Template, Values, ValuesFn};

macro_rules! make_values {
    ($($name:expr => $value:expr),*) => {
        (
            &[$(($name, $value)),*],
            {
                let mut map = HashMap::new();
                $(
                    map.insert($name, $value);
                )*
                map
            },
            vals(|key| match key {
                $(
                    $name => Some(Cow::Borrowed($value)),
                )*
                _ => None,
            })
        )
    };
}

fn one_replace(c: &mut Criterion) {
    const TEMPLATE: &str = "Hello, {name}!";

    let (slice, hashmap, vals) = make_values!(
        "name" => "marcus"
    );

    inner_bench("one replace", c, TEMPLATE, vals, hashmap, slice);
}

fn some_replaces(c: &mut Criterion) {
    const TEMPLATE: &str = "hello {name}! i am {age} years old. my goal is to {goal}. i like: {flower}, {music}, {animal}, {color}, {food}. i'm drinking {drink}";

    let (slice, hashmap, vals) = make_values!(
        "name" => "marcus",
        "age" => "42",
        "goal" => "primary",
        "flower" => "lotus",
        "music" => "jazz",
        "animal" => "cat",
        "color" => "blue",
        "food" => "pizza",
        "drink" => "coffee"
    );

    inner_bench("some replaces", c, TEMPLATE, vals, hashmap, slice);
}

fn many_replaces(c: &mut Criterion) {
    const TEMPLATE: &str = "
        {artichoke}
        {aubergine}
        {asparagus}
        {broccoflower}
        {broccoli}
        {brussels sprouts}
        {cabbage}
        {kohlrabi}
        {Savoy cabbage}
        {red cabbage}
        {cauliflower}
        {celery}
        {endive}
        {fiddleheads}
        {frisee}
        {fennel}
        {greens}
        {arugula}
        {bok choy}
        {chard}
        {collard greens}
        {kale}
        {lettuce}
        {mustard greens}
        {spinach}
        {herbs}
        {anise}
        {basil}
        {caraway}
        {coriander}
        {chamomile}
        {daikon}
        {dill}
        {squash}
        {lavender}
        {cymbopogon}
        {marjoram}
        {oregano}
        {parsley}
        {rosemary}
        {thyme}
        {legumes}
        {alfalfa sprouts}
        {azuki beans}
        {bean sprouts}
        {black beans}
        {black-eyed peas}
        {borlotti bean}
        {broad beans}
        {chickpeas, garbanzos, or ceci beans}
        {green beans}
        {kidney beans}
        {lentils}
        {lima beans or butter bean}
        {mung beans}
        {navy beans}
        {peanuts}
        {pinto beans}
        {runner beans}
        {split peas}
        {soy beans}
        {peas}
        {mange tout or snap peas}
        {mushrooms}
        {nettles}
        {New Zealand spinach}
        {okra}
        {onions}
        {chives}
        {garlic}
        {leek}
        {onion}
        {shallot}
        {scallion}
        {peppers}
        {bell pepper}
        {chili pepper}
        {jalapeño}
        {habanero}
        {paprika}
        {tabasco pepper}
        {cayenne pepper}
        {radicchio}
        {rhubarb}
        {root vegetables}
        {beetroot}
        {beet}
        {mangelwurzel}
        {carrot}
        {celeriac}
        {corms}
        {eddoe}
        {konjac}
        {taro}
        {water chestnut}
        {ginger}
        {parsnip}
        {rutabaga}
        {radish}
        {wasabi}
    ";

    let (slice, hashmap, vals) = make_values!(
        "artichoke" => "Abiu",
        "aubergine" => "Açaí",
        "asparagus" => "Acerola",
        "broccoflower" => "Akebi",
        "broccoli" => "Ackee",
        "brussels sprouts" => "African Cherry Orange",
        "cabbage" => "American Mayapple",
        "kohlrabi" => "Apple",
        "Savoy cabbage" => "Apricot",
        "red cabbage" => "Araza",
        "cauliflower" => "Avocado",
        "celery" => "Banana",
        "endive" => "Bilberry",
        "fiddleheads" => "Blackberry",
        "frisee" => "Blackcurrant",
        "fennel" => "Black sapote",
        "greens" => "Blueberry",
        "arugula" => "Boysenberry",
        "bok choy" => "Breadfruit",
        "chard" => "Buddha's hand",
        "collard greens" => "Cactus pear",
        "kale" => "Canistel",
        "lettuce" => "Cashew",
        "mustard greens" => "Cempedak",
        "spinach" => "Cherimoya",
        "herbs" => "Cherry",
        "anise" => "Chico fruit",
        "basil" => "Cloudberry",
        "caraway" => "Coco de mer",
        "coriander" => "Coconut",
        "chamomile" => "Crab apple",
        "daikon" => "Cranberry",
        "dill" => "Currant",
        "squash" => "Damson",
        "lavender" => "Date",
        "cymbopogon" => "Dragonfruit",
        "marjoram" => "Pitaya",
        "oregano" => "Durian",
        "parsley" => "Elderberry",
        "rosemary" => "Feijoa",
        "thyme" => "Fig",
        "legumes" => "Finger Lime",
        "alfalfa sprouts" => "Caviar Lime",
        "azuki beans" => "Goji berry",
        "bean sprouts" => "Gooseberry",
        "black beans" => "Grape",
        "black-eyed peas" => "Raisin",
        "borlotti bean" => "Grapefruit",
        "broad beans" => "Grewia asiatica",
        "chickpeas, garbanzos, or ceci beans" => "Guava",
        "green beans" => "Hala Fruit",
        "kidney beans" => "Honeyberry",
        "lentils" => "Huckleberry",
        "lima beans or butter bean" => "Jabuticaba",
        "mung beans" => "Jackfruit",
        "navy beans" => "Jambul",
        "peanuts" => "Japanese plum",
        "pinto beans" => "Jostaberry",
        "runner beans" => "Jujube",
        "split peas" => "Juniper berry",
        "soy beans" => "Kaffir Lime",
        "peas" => "Kiwano",
        "mange tout or snap peas" => "Kiwifruit",
        "mushrooms" => "Kumquat",
        "nettles" => "Lemon",
        "New Zealand spinach" => "Lime",
        "okra" => "Loganberry",
        "onions" => "Longan",
        "chives" => "Loquat",
        "garlic" => "Lulo",
        "leek" => "Lychee",
        "onion" => "Magellan Barberry",
        "shallot" => "Mamey Apple",
        "scallion" => "Mamey Sapote",
        "peppers" => "Mango",
        "bell pepper" => "Mangosteen",
        "chili pepper" => "Marionberry",
        "jalapeño" => "Melon",
        "habanero" => "Cantaloupe",
        "paprika" => "Galia melon",
        "tabasco pepper" => "Honeydew",
        "cayenne pepper" => "Mouse melon",
        "radicchio" => "Musk melon",
        "rhubarb" => "Watermelon",
        "root vegetables" => "Miracle fruit",
        "beetroot" => "Momordica fruit",
        "beet" => "Monstera deliciosa",
        "mangelwurzel" => "Mulberry",
        "carrot" => "Nance",
        "celeriac" => "Nectarine",
        "corms" => "Orange",
        "eddoe" => "Blood orange",
        "konjac" => "Clementine",
        "taro" => "Mandarine",
        "water chestnut" => "Tangerine",
        "ginger" => "Papaya",
        "parsnip" => "Passionfruit",
        "rutabaga" => "Pawpaw",
        "radish" => "Peach",
        "wasabi" => "Pear"
    );

    inner_bench("many replaces", c, TEMPLATE, vals, hashmap, slice);
}

fn inner_bench<F>(
    name: &str,
    c: &mut Criterion,
    template_str: &str,
    vals: ValuesFn<F>,
    hashmap: HashMap<&str, &str>,
    slice: &[(&str, &str)],
) where
    F: Fn(&str) -> Option<Cow<'static, str>> + Send + Clone + 'static,
{
    c.bench_function(&format!("{name}, fn"), move |b| {
        let vals = vals.clone();
        b.iter(move || {
            let template = Template::parse(black_box(template_str)).unwrap();
            black_box(template.render(&vals).unwrap());
        })
    });
    let hashmap = Arc::new(hashmap);
    c.bench_function(&format!("{name}, hashmap"), move |b| {
        let hashmap = Arc::clone(&hashmap);
        b.iter(move || {
            let template = Template::parse(black_box(template_str)).unwrap();
            black_box(template.render(&hashmap).unwrap());
        })
    });
    c.bench_function(&format!("{name}, slice"), move |b| {
        b.iter(move || {
            let template = Template::parse(black_box(template_str)).unwrap();
            black_box(template.render(&slice as &dyn Values).unwrap());
        })
    });
}

criterion_group!(one, one_replace);
criterion_group!(some, some_replaces);
criterion_group!(many, many_replaces);
criterion_main!(one, some, many);
