"""Create owned synthetic test PNGs; never imports/decodes submitted images.

Requires Pillow and an installed trusted CJK font; no font files are copied.
Use --font with NotoSansCJK-Regular.ttc, or Windows Microsoft YaHei.
"""
import argparse
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

parser=argparse.ArgumentParser()
parser.add_argument('--font',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
args.output.mkdir(parents=True,exist_ok=True)
font=ImageFont.truetype(str(args.font),72)
for name,text in [('ocr-english','TASKLOOM OCR 123'),('ocr-chinese','任务测试 成功')]:
    image=Image.new('RGB',(1200,240),'white')
    ImageDraw.Draw(image).text((50,65),text,font=font,fill='black')
    image.save(args.output/(name+'.png'),format='PNG')
image=Image.new('RGB',(2000,3000),'white')
draw=ImageDraw.Draw(image)
small=ImageFont.truetype(str(args.font),40)
for row in range(50):
    draw.text((30,20+row*58),'TASKLOOM OCR cancellation and bounded output 123',font=small,fill='black')
image.save(args.output/'ocr-long.png',format='PNG')
