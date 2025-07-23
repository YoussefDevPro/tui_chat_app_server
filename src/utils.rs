use rand::Rng;

pub fn parse_and_calculate(expression: &str) -> String {
    let ops = ['+', '-', '*', '/'];
    for op in ops.iter() {
        if expression.contains(*op) {
            let parts: Vec<&str> = expression.split(*op).collect();
            if parts.len() == 2 {
                if let (Ok(num1), Ok(num2)) = (
                    parts[0].trim().parse::<f64>(),
                    parts[1].trim().parse::<f64>(),
                ) {
                    return match op {
                        '+' => (num1 + num2).to_string(),
                        '-' => (num1 - num2).to_string(),
                        '*' => (num1 * num2).to_string(),
                        '/' => {
                            if num2 != 0.0 {
                                (num1 / num2).to_string()
                            } else {
                                "Error: Division by zero".to_string()
                            }
                        }
                        _ => "Error: Invalid operator".to_string(),
                    };
                }
            }
        }
    }
    "Error: Invalid expression format. Use 'num1 operator num2' (e.g., 5+3)".to_string()
}

pub fn parse_and_roll_dice(input: &str) -> String {
    let lowercased_input = input.to_lowercase();
    let parts: Vec<&str> = lowercased_input.split('d').collect();
    if parts.len() == 2 {
        if let (Ok(num_dice), Ok(num_sides)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            if num_dice == 0 || num_sides == 0 {
                return "Error: Number of dice or sides cannot be zero.".to_string();
            }
            if num_dice > 100 || num_sides > 1000 {
                return "Error: Too many dice or sides. Max 100d1000.".to_string();
            }

            let mut total_roll = 0;
            let mut rng = rand::rngs::ThreadRng::default();
            let mut individual_rolls = Vec::new();
            for _ in 0..num_dice {
                let roll: u32 = rng.gen_range(1..=num_sides);
                total_roll += roll;
                individual_rolls.push(roll.to_string());
            }
            return format!("{} (rolls: {})", total_roll, individual_rolls.join(", "));
        }
    }
    "Error: Invalid dice format. Use 'NdM' (e.g., 2d6)".to_string()
}