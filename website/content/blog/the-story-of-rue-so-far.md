+++
title = "The Story of Rue So Far"
date = 2025-12-22
template = "blog-page.html"

[extra]
authors = ["steve"]
+++

Hi, Steve here, no Claude. I wanted to talk about what I've done so far with Rue. I plan on letting Claude also write something soon as well, but I wanted to get my own words down first.

<!-- more -->

I've always thought compilers were cool. When I was in college, I structured my classes around how to get to compilers as quickly as possible. I wanted to know how they worked! I had written code in a bunch of languages, and the process of making them just seemed very neat to me.

Professionally, though, it wasn't clear how to pursue this path. And this whole Ruby on Rails thing was blowing up... so I ended up working on web stuff instead. Don't get me wrong, that was also very fulfilling, but it was also very different.

That's part of why, years later, I decided to work on Rust. I've just always thought languages were great. But when looking at how I could help Rust succeed, I knew that working on the compiler wasn't the highest leverage thing that I could do. So I didn't. I focused on other things that I also enjoy, and am good at.

It's been a few years since I've worked on Rust. And I've been missing that in my life. It was tremendously fun to contribute to a language! It was hard, and heartbreaking at times... but also great.

## Languages are a team sport

One thing it did hammer home for me though, is how much language design and implementation is a team sport. A lot of the languages that started before 2000 were hobby projects that made it big. But a lot of languages that were successful since then have had corporate sponsorship, and big teams. A lot of people have been wondering if you can realistically build a language as a hobby anymore. The expectations are just so much higher now: package managers and linters and an LSP and editor integration, all of this just wasn't considered a requirement back in the day, but now feels like table stakes. And that's a lot of work!

So, I hadn't been bothering to work on a language project. Because I knew that I'm getting old, and I just don't have the time anymore. And 2025 has given me even less time, thanks to changes in my life. Good changes! But changes. In my 20s, I thought, "wow, how can people say they can't find the time to contribute to open source?" and now I barely have time for video games, let alone open source.

## Leverage

But 2025 also brought one more change in my life: I went from thinking that AI and LLMs were stupid and bad, to being at least useful. For the purposes of this post, I don't really want to get into the details, because they're not relevant, but I'm sure I'll write more about that elsewhere. But as I started to seriously engage with them, an idea kept popping up in the back of my head:

What if Claude could write a compiler?

I was slowly using Claude for more and more things, in terms of writing software. I thought I was getting pretty good at it. So, why not give it a try? Because really, what I need to get projects done is leverage. I only have a limited amount of time, and I need to get a *lot* done. That's what LLMs are supposed to help with, right?

So one day, I gave it a shot. And you know what? It worked. It was a bit janky, and in fact, its first run of the program it compiled didn't work. But then it debugged that and fixed it.

Maybe this would actually work?

## Too many vibes

You can find out how that went by looking at the [first-attempt branch][first-attempt]. It went okay! But it also went off the rails a bit. But that was also okay with me! I was trying to stretch here. And so I overly vibed it. I didn't really do proper engineering, I just kinda did stuff. And doing so taught me a ton! I don't believe LLMs are magic. I think they require certain engineering practices to really do well with them. But I was learning those practices.

Just as I was kind of hitting a wall here, I ran into a month and a half of intense travel. So I kind of just stopped for a bit. Which is always okay for a side project! And it also would have been okay with me if this experiment just stopped there entirely.

## Second time's the charm

However, last week, I had some spare time... and I decided to start over. I have a lot more skill with Claude than I did half a year ago. And I have a much better idea of what I liked, what I didn't like, what was working, and what didn't.

So I started over.

And it's going *way* better this time! Technically, Rue doesn't have all of the features that it had, but in many ways, it has more already too. And I feel much better about the state of the codebase, the velocity of the project, and my ability to keep working on it in my limited spare time.

So that's why I decided to more formally announce things. I'm more ready to share this project with the world than I was back then. And I wanted to make this blog to write some of this out. I'm sure some of you are asking "Well what *are* those practices? Why is it going better this time?" And I do want to write that stuff up. But not for tonight.

Tonight, I'd just like to bask in the fact that I got a baby language from zero to "core basics of a language + spec with two different codegen backends" done in roughly a week." That's wild to me!

And maybe I'll run out of steam again, or maybe I'll hit a wall with Claude. Who knows. But I've already had a tremendous amount of fun with all of this. And hopefully you all can learn from my success or failure here. We'll see which way it goes.

[first-attempt]: https://github.com/rue-language/rue/tree/first-attempt