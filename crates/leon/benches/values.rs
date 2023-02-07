use std::collections::HashMap;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use leon::{vals, Template};

fn one_replace(c: &mut Criterion) {
    const TEMPLATE: &str = "Hello, {name}!";
    let mut hashmap = HashMap::new();
    hashmap.insert("name", "marcus");

    let slice = &[("name", "marcus")];

    c.bench_function("one replace, fn", |b| {
        b.iter(|| {
            let template = Template::parse(black_box(TEMPLATE)).unwrap();
            black_box(template.render(&vals(|_| Some("marcus"))).unwrap());
        })
    });
    c.bench_function("one replace, hashmap", |b| {
        b.iter(|| {
            let template = Template::parse(black_box(TEMPLATE)).unwrap();
            black_box(template.render(&hashmap).unwrap());
        })
    });
    c.bench_function("one replace, slice", |b| {
        b.iter(|| {
            let template = Template::parse(black_box(TEMPLATE)).unwrap();
            black_box(template.render(&slice).unwrap());
        })
    });
}

fn some_replaces(c: &mut Criterion) {
    const TEMPLATE: &str = "hello {name}! i am {age} years old. my goal is to {goal}. i like: {flower}, {music}, {animal}, {color}, {food}. i'm drinking {drink}";
    let mut hashmap = HashMap::new();
    hashmap.insert("name", "marcus");
    hashmap.insert("age", "42");
    hashmap.insert("goal", "primary");
    hashmap.insert("flower", "lotus");
    hashmap.insert("music", "jazz");
    hashmap.insert("animal", "cat");
    hashmap.insert("color", "blue");
    hashmap.insert("food", "pizza");
    hashmap.insert("drink", "coffee");

    let slice = &[
        ("name", "marcus"),
        ("age", "42"),
        ("goal", "primary"),
        ("flower", "lotus"),
        ("music", "jazz"),
        ("animal", "cat"),
        ("color", "blue"),
        ("food", "pizza"),
        ("drink", "coffee"),
    ];

    c.bench_function("some replaces, fn", |b| {
        b.iter(|| {
            let template = Template::parse(black_box(TEMPLATE)).unwrap();
            black_box(
                template
                    .render(&vals(|key| match key {
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
                    }))
                    .unwrap(),
            );
        })
    });
    c.bench_function("some replaces, hashmap", |b| {
        b.iter(|| {
            let template = Template::parse(black_box(TEMPLATE)).unwrap();
            black_box(template.render(&hashmap).unwrap());
        })
    });
    c.bench_function("some replaces, slice", |b| {
        b.iter(|| {
            let template = Template::parse(black_box(TEMPLATE)).unwrap();
            black_box(template.render(&slice).unwrap());
        })
    });
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
    let mut hashmap = HashMap::new();
    hashmap.insert("artichoke", "Abiu");
hashmap.insert("aubergine", "Açaí");
hashmap.insert("asparagus", "Acerola");
hashmap.insert("broccoflower", "Akebi");
hashmap.insert("broccoli", "Ackee");
hashmap.insert("brussels sprouts", "African Cherry Orange");
hashmap.insert("cabbage", "American Mayapple");
hashmap.insert("kohlrabi", "Apple");
hashmap.insert("Savoy cabbage", "Apricot");
hashmap.insert("red cabbage", "Araza");
hashmap.insert("cauliflower", "Avocado");
hashmap.insert("celery", "Banana");
hashmap.insert("endive", "Bilberry");
hashmap.insert("fiddleheads", "Blackberry");
hashmap.insert("frisee", "Blackcurrant");
hashmap.insert("fennel", "Black sapote");
hashmap.insert("greens", "Blueberry");
hashmap.insert("arugula", "Boysenberry");
hashmap.insert("bok choy", "Breadfruit");
hashmap.insert("chard", "Buddha's hand");
hashmap.insert("collard greens", "Cactus pear");
hashmap.insert("kale", "Canistel");
hashmap.insert("lettuce", "Cashew");
hashmap.insert("mustard greens", "Cempedak");
hashmap.insert("spinach", "Cherimoya");
hashmap.insert("herbs", "Cherry");
hashmap.insert("anise", "Chico fruit");
hashmap.insert("basil", "Cloudberry");
hashmap.insert("caraway", "Coco de mer");
hashmap.insert("coriander", "Coconut");
hashmap.insert("chamomile", "Crab apple");
hashmap.insert("daikon", "Cranberry");
hashmap.insert("dill", "Currant");
hashmap.insert("squash", "Damson");
hashmap.insert("lavender", "Date");
hashmap.insert("cymbopogon", "Dragonfruit");
hashmap.insert("marjoram", "Pitaya");
hashmap.insert("oregano", "Durian");
hashmap.insert("parsley", "Elderberry");
hashmap.insert("rosemary", "Feijoa");
hashmap.insert("thyme", "Fig");
hashmap.insert("legumes", "Finger Lime");
hashmap.insert("alfalfa sprouts", "Caviar Lime");
hashmap.insert("azuki beans", "Goji berry");
hashmap.insert("bean sprouts", "Gooseberry");
hashmap.insert("black beans", "Grape");
hashmap.insert("black-eyed peas", "Raisin");
hashmap.insert("borlotti bean", "Grapefruit");
hashmap.insert("broad beans", "Grewia asiatica");
hashmap.insert("chickpeas, garbanzos, or ceci beans", "Guava");
hashmap.insert("green beans", "Hala Fruit");
hashmap.insert("kidney beans", "Honeyberry");
hashmap.insert("lentils", "Huckleberry");
hashmap.insert("lima beans or butter bean", "Jabuticaba");
hashmap.insert("mung beans", "Jackfruit");
hashmap.insert("navy beans", "Jambul");
hashmap.insert("peanuts", "Japanese plum");
hashmap.insert("pinto beans", "Jostaberry");
hashmap.insert("runner beans", "Jujube");
hashmap.insert("split peas", "Juniper berry");
hashmap.insert("soy beans", "Kaffir Lime");
hashmap.insert("peas", "Kiwano");
hashmap.insert("mange tout or snap peas", "Kiwifruit");
hashmap.insert("mushrooms", "Kumquat");
hashmap.insert("nettles", "Lemon");
hashmap.insert("New Zealand spinach", "Lime");
hashmap.insert("okra", "Loganberry");
hashmap.insert("onions", "Longan");
hashmap.insert("chives", "Loquat");
hashmap.insert("garlic", "Lulo");
hashmap.insert("leek", "Lychee");
hashmap.insert("onion", "Magellan Barberry");
hashmap.insert("shallot", "Mamey Apple");
hashmap.insert("scallion", "Mamey Sapote");
hashmap.insert("peppers", "Mango");
hashmap.insert("bell pepper", "Mangosteen");
hashmap.insert("chili pepper", "Marionberry");
hashmap.insert("jalapeño", "Melon");
hashmap.insert("habanero", "Cantaloupe");
hashmap.insert("paprika", "Galia melon");
hashmap.insert("tabasco pepper", "Honeydew");
hashmap.insert("cayenne pepper", "Mouse melon");
hashmap.insert("radicchio", "Musk melon");
hashmap.insert("rhubarb", "Watermelon");
hashmap.insert("root vegetables", "Miracle fruit");
hashmap.insert("beetroot", "Momordica fruit");
hashmap.insert("beet", "Monstera deliciosa");
hashmap.insert("mangelwurzel", "Mulberry");
hashmap.insert("carrot", "Nance");
hashmap.insert("celeriac", "Nectarine");
hashmap.insert("corms", "Orange");
hashmap.insert("eddoe", "Blood orange");
hashmap.insert("konjac", "Clementine");
hashmap.insert("taro", "Mandarine");
hashmap.insert("water chestnut", "Tangerine");
hashmap.insert("ginger", "Papaya");
hashmap.insert("parsnip", "Passionfruit");
hashmap.insert("rutabaga", "Pawpaw");
hashmap.insert("radish", "Peach");
hashmap.insert("wasabi", "Pear");

    let slice = hashmap
        .iter()
        .map(|(k, v)| (*k, *v))
        .collect::<Vec<(&str, &str)>>();

    c.bench_function("many replaces, fn", |b| {
        b.iter(|| {
            let template = Template::parse(black_box(TEMPLATE)).unwrap();
            black_box(
                template
                    .render(&vals(|key| {
                        Some(match key {
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
                            "wasabi" => "Pear",
                            _ => return None,
                        })
                    }))
                    .unwrap(),
            );
        })
    });
    c.bench_function("many replaces, hashmap", |b| {
        b.iter(|| {
            let template = Template::parse(black_box(TEMPLATE)).unwrap();
            black_box(template.render(&hashmap).unwrap());
        })
    });
    c.bench_function("many replaces, slice", |b| {
        b.iter(|| {
            let template = Template::parse(black_box(TEMPLATE)).unwrap();
            black_box(template.render(&slice).unwrap());
        })
    });
}

criterion_group!(one, one_replace);
criterion_group!(some, some_replaces);
criterion_group!(many, many_replaces);
criterion_main!(one, some, many);
